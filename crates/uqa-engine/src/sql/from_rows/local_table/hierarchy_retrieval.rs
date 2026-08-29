//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scored retrieval over ordinary-inheritance and partition hierarchies.

use std::collections::BTreeSet;
use std::sync::Arc;

use super::{
    qualify_source_operator, table_lock_origin, ColumnPrune, CteScope, Engine,
    HierarchyScoredDocumentSource, SQLError, SQLParam, ScalarExpr, ScoredDocumentSource,
    ScoredInput, SharedLockOrigin, SourcePlan, TABLE_OID_COLUMN, XMIN_COLUMN,
};
use crate::sql::select::RecheckDoc;
use crate::ScoredEntry;
use uqa_execution::PhysicalOperator;
use uqa_operators::RelevantSampleSplit;

struct PhysicalRetrieval {
    table_name: String,
    entries: Vec<ScoredEntry>,
    lock_origin: Option<SharedLockOrigin>,
    recheck_pins: Option<Arc<Vec<RecheckDoc>>>,
}

pub(super) fn build_hierarchy_retrieval_operator<'a>(
    engine: &'a Engine,
    source: &SourcePlan,
    qualifier: &str,
    predicate: &ScalarExpr,
    params: &'a [SQLParam],
    ctes: &CteScope,
    prune: Option<&ColumnPrune>,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    let SourcePlan::Table {
        name: logical_table,
        include_descendants,
        ..
    } = source
    else {
        return Err(SQLError::Internal(
            "hierarchy retrieval requires a table source".into(),
        ));
    };
    let direct_vector =
        crate::operator_tree_bridge::direct_vector_retrieval(engine, predicate, params)?;
    let table_names = engine.hierarchy_scan_tables(logical_table, *include_descendants)?;
    let mut physical = Vec::with_capacity(table_names.len());
    for table_name in table_names {
        let lock_origin =
            table_lock_origin(engine, &table_name, qualifier, ctes.lock_identities.emit)?;
        let recheck_pins = lock_origin
            .as_ref()
            .and_then(|(origin_qualifier, storage_name)| {
                ctes.recheck_docs_for_scan(origin_qualifier, storage_name)
            });
        let entries = if let Some(
            crate::operator_tree_bridge::DirectVectorRetrieval::Calibrated {
                field,
                query_vector,
                top_k,
                ..
            },
        ) = &direct_vector
        {
            if recheck_pins.is_some() {
                engine.committed_knn_entries(&table_name, field, query_vector, *top_k)?
            } else {
                engine.knn_search_leaf(&table_name, field, query_vector, *top_k)?
            }
        } else {
            let entries = if recheck_pins.is_some() {
                engine.committed_retrieval_entries(&table_name, predicate, params)?
            } else {
                crate::operator_tree_bridge::run_optimised(
                    engine,
                    &table_name,
                    Some(predicate),
                    params,
                )?
            };
            entries.ok_or_else(|| {
                SQLError::Unsupported(format!(
                    "JOIN filter retrieval predicate for `{qualifier}` cannot be represented by the shared operator IR"
                ))
            })?
        };
        physical.push(PhysicalRetrieval {
            table_name,
            entries,
            lock_origin,
            recheck_pins,
        });
    }

    match direct_vector {
        Some(crate::operator_tree_bridge::DirectVectorRetrieval::Knn { top_k }) => {
            retain_global_top_k(&mut physical, top_k);
        }
        Some(crate::operator_tree_bridge::DirectVectorRetrieval::Calibrated {
            top_k,
            threshold,
            ..
        }) => {
            retain_global_top_k(&mut physical, top_k);
            calibrate_global_vector_pool(&mut physical, threshold)?;
        }
        None => {}
    }

    let mut columns = engine.try_table_columns(logical_table).map_err(|error| {
        SQLError::Internal(format!("read table columns for `{logical_table}`: {error}"))
    })?;
    if prune
        .and_then(|prune| prune.get(qualifier))
        .is_some_and(|wanted| wanted.contains(TABLE_OID_COLUMN))
    {
        columns.push(TABLE_OID_COLUMN.into());
    }
    if prune
        .and_then(|prune| prune.get(qualifier))
        .is_some_and(|wanted| wanted.contains(XMIN_COLUMN))
    {
        columns.push(XMIN_COLUMN.into());
    }
    let metadata = prune
        .and_then(|prune| prune.get(qualifier))
        .map(super::super::SourceProjection::metadata)
        .unwrap_or_default();
    let estimated_cardinality = physical
        .iter()
        .map(|retrieval| retrieval.entries.len())
        .sum();
    let score_column = uqa_sql::ast::InternalRelationId::allocate().column(0);
    let mut sources = Vec::with_capacity(physical.len());
    for retrieval in physical {
        let table = engine.require_table(&retrieval.table_name)?;
        sources.push(
            ScoredDocumentSource::new_configured(
                &retrieval.table_name,
                table,
                ScoredInput::entries(retrieval.entries, true),
                columns.clone(),
                None,
                None,
                super::ScoredSourceAttributes::shared_score(score_column, metadata),
            )
            .with_table_oid(crate::sql::catalog::table_relation_oid(
                engine,
                &retrieval.table_name,
            )?)
            .with_lock_origin(retrieval.lock_origin)
            .with_recheck_pins(retrieval.recheck_pins),
        );
    }
    let source: Box<dyn uqa_execution::RowSource> = if sources.len() == 1 {
        Box::new(
            sources
                .pop()
                .ok_or_else(|| SQLError::Internal("single retrieval source was lost".into()))?,
        )
    } else {
        Box::new(HierarchyScoredDocumentSource::new(
            sources,
            estimated_cardinality,
        )?)
    };
    let scan: Box<dyn PhysicalOperator + 'a> = Box::new(uqa_execution::TableScan::new(source));
    Ok(qualify_source_operator(
        scan,
        qualifier,
        prune,
        ctes.lock_identities.emit,
    ))
}

fn retain_global_top_k(physical: &mut [PhysicalRetrieval], top_k: usize) {
    let mut ranked = physical
        .iter()
        .enumerate()
        .flat_map(|(table_position, retrieval)| {
            retrieval
                .entries
                .iter()
                .enumerate()
                .map(move |(entry_position, entry)| {
                    (table_position, entry_position, entry.doc_id, entry.score)
                })
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .3
            .total_cmp(&left.3)
            .then_with(|| left.0.cmp(&right.0))
            .then_with(|| left.2.cmp(&right.2))
    });
    ranked.truncate(top_k);
    let retained = ranked
        .into_iter()
        .map(|(table_position, entry_position, _, _)| (table_position, entry_position))
        .collect::<BTreeSet<_>>();
    for (table_position, retrieval) in physical.iter_mut().enumerate() {
        let mut entry_position = 0usize;
        retrieval.entries.retain(|_| {
            let keep = retained.contains(&(table_position, entry_position));
            entry_position += 1;
            keep
        });
    }
}

fn calibrate_global_vector_pool(
    physical: &mut [PhysicalRetrieval],
    threshold: Option<f64>,
) -> Result<(), SQLError> {
    let distances = physical
        .iter()
        .flat_map(|retrieval| retrieval.entries.iter())
        .map(|entry| 1.0 - entry.score)
        .collect::<Vec<_>>();
    let transform =
        uqa_operators::fit_pool_calibration(&distances, RelevantSampleSplit::default(), 0.5)
            .map_err(|error| {
                SQLError::Internal(format!("calibrate hierarchy vector pool: {error}"))
            })?;
    let mut distance = distances.into_iter();
    for retrieval in physical {
        for entry in &mut retrieval.entries {
            let value = distance.next().ok_or_else(|| {
                SQLError::Internal("hierarchy vector calibration lost a candidate".into())
            })?;
            entry.score = transform
                .as_ref()
                .map_or(Ok(0.5), |transform| {
                    transform.calibrate_one(value).map_err(|error| {
                        SQLError::Internal(format!("calibrate hierarchy vector candidate: {error}"))
                    })
                })?
                .clamp(1e-6, 1.0 - 1e-6);
        }
        retrieval
            .entries
            .retain(|entry| threshold.is_none_or(|minimum| entry.score >= minimum));
    }
    if distance.next().is_some() {
        return Err(SQLError::Internal(
            "hierarchy vector calibration left an unmatched candidate".into(),
        ));
    }
    Ok(())
}
