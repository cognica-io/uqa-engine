//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind retrieval metadata eagerly and execute the selected snapshot only when a consumer requests rows.

use super::{
    calibrate_global_vector_pool, retain_global_top_k, DirectVectorRetrieval, PhysicalRetrieval,
};
use crate::query::{
    deferred::DeferredOperator,
    scored_input::{HierarchyScoredDocumentSource, ProducedScoredEntries, ScoredDocumentSource},
    table_sources::retrieval::RetrievalAccess,
};
use crate::{RowSchema, RowSource};
use uqa_sql::{SQLError, SQLParam, ScalarExpr};

pub(super) fn deferred<'a>(
    retrieval: &'a dyn RetrievalAccess,
    physical: Vec<PhysicalRetrieval>,
    direct: Option<DirectVectorRetrieval>,
    predicate: ScalarExpr,
    params: &'a [SQLParam],
    sources: Vec<ScoredDocumentSource>,
    schema: RowSchema,
) -> DeferredOperator<'a> {
    DeferredOperator::from_rows(
        schema,
        Box::new(move || evaluate(retrieval, physical, direct, predicate, params, sources)),
    )
}

fn evaluate(
    retrieval: &dyn RetrievalAccess,
    mut physical: Vec<PhysicalRetrieval>,
    direct: Option<DirectVectorRetrieval>,
    predicate: ScalarExpr,
    params: &[SQLParam],
    sources: Vec<ScoredDocumentSource>,
) -> Result<Box<dyn RowSource>, SQLError> {
    let inputs = if let Some(direct) = direct.as_ref() {
        evaluate_vector(retrieval, &mut physical, direct, &predicate, params)?;
        physical
            .iter_mut()
            .map(|target| std::mem::take(&mut target.entries).into())
            .collect()
    } else {
        physical
            .iter()
            .map(|target| {
                retrieval
                    .retrieval_input(
                        &target.table_name,
                        &predicate,
                        params,
                        target.recheck_pins.is_some(),
                    )?
                    .ok_or_else(unrepresentable_predicate)
            })
            .collect::<Result<Vec<_>, SQLError>>()?
    };
    let estimated_cardinality = inputs
        .iter()
        .map(ProducedScoredEntries::estimated_cardinality)
        .sum();
    let mut sources = sources
        .into_iter()
        .zip(physical)
        .zip(inputs)
        .map(|((source, target), input)| {
            let source = match input {
                ProducedScoredEntries::Materialized(entries) => {
                    source.with_retrieval_entries(entries)
                }
                ProducedScoredEntries::Captured(entries) => source.with_captured_entries(entries),
            };
            source.with_recheck_pins(target.recheck_pins)
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    let source: Box<dyn RowSource> = if sources.len() == 1 {
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
    Ok(source)
}

fn unrepresentable_predicate() -> SQLError {
    SQLError::Unsupported(
        "retrieval predicate cannot be represented by the shared operator IR".into(),
    )
}

fn evaluate_vector(
    retrieval: &dyn RetrievalAccess,
    physical: &mut [PhysicalRetrieval],
    direct: &DirectVectorRetrieval,
    predicate: &ScalarExpr,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    for target in physical.iter_mut() {
        target.entries = if let DirectVectorRetrieval::Calibrated {
            field,
            query_vector,
            top_k,
            ..
        } = direct
        {
            retrieval.knn_entries(
                &target.table_name,
                field,
                query_vector,
                *top_k,
                target.recheck_pins.is_some(),
            )?
        } else {
            retrieval
                .retrieval_entries(
                    &target.table_name,
                    predicate,
                    params,
                    target.recheck_pins.is_some(),
                )?
                .ok_or_else(unrepresentable_predicate)?
        };
    }
    match direct {
        DirectVectorRetrieval::Knn { top_k } => {
            retain_global_top_k(physical, *top_k);
        }
        DirectVectorRetrieval::Calibrated {
            top_k, threshold, ..
        } => {
            retain_global_top_k(physical, *top_k);
            calibrate_global_vector_pool(physical, *threshold)?;
        }
    }
    Ok(())
}
