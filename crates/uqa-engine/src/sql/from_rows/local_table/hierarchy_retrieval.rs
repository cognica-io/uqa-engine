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
    ScoredInput, SharedLockOrigin, SourcePlan,
};
use crate::sql::select::RecheckDoc;
use crate::ScoredEntry;
use uqa_execution::PhysicalOperator;

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
        let entries = if recheck_pins.is_some() {
            engine.committed_retrieval_entries(&table_name, predicate, params)?
        } else {
            crate::operator_tree_bridge::run_optimised(
                engine,
                &table_name,
                Some(predicate),
                params,
            )?
        }
        .ok_or_else(|| {
            SQLError::Unsupported(format!(
                "JOIN filter retrieval predicate for `{qualifier}` cannot be represented by the shared operator IR"
            ))
        })?;
        physical.push(PhysicalRetrieval {
            table_name,
            entries,
            lock_origin,
            recheck_pins,
        });
    }

    if let Some(top_k) =
        crate::operator_tree_bridge::direct_knn_support_limit(engine, predicate, params)?
    {
        retain_global_top_k(&mut physical, top_k);
    }

    let columns = engine.try_table_columns(logical_table).map_err(|error| {
        SQLError::Internal(format!("read table columns for `{logical_table}`: {error}"))
    })?;
    let estimated_cardinality = physical
        .iter()
        .map(|retrieval| retrieval.entries.len())
        .sum();
    let mut sources = Vec::with_capacity(physical.len());
    for retrieval in physical {
        let table = engine.require_table(&retrieval.table_name)?;
        sources.push(
            ScoredDocumentSource::new(
                &retrieval.table_name,
                table,
                ScoredInput::entries(retrieval.entries, true),
                columns.clone(),
                None,
                None,
            )
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
