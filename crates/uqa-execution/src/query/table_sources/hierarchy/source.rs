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
    scored_input::{DeferredTableScan, HierarchyScoredDocumentSource, ScoredDocumentSource},
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
) -> DeferredTableScan<'a> {
    DeferredTableScan::new(
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
    for target in &mut physical {
        target.entries = if let Some(DirectVectorRetrieval::Calibrated {
            field,
            query_vector,
            top_k,
            ..
        }) = &direct
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
                    &predicate,
                    params,
                    target.recheck_pins.is_some(),
                )?
                .ok_or_else(|| {
                    SQLError::Unsupported(
                        "retrieval predicate cannot be represented by the shared operator IR"
                            .into(),
                    )
                })?
        };
    }
    match &direct {
        Some(DirectVectorRetrieval::Knn { top_k }) => {
            retain_global_top_k(&mut physical, *top_k);
        }
        Some(DirectVectorRetrieval::Calibrated {
            top_k, threshold, ..
        }) => {
            retain_global_top_k(&mut physical, *top_k);
            calibrate_global_vector_pool(&mut physical, *threshold)?;
        }
        None => {}
    }
    let estimated_cardinality = physical.iter().map(|target| target.entries.len()).sum();
    let mut sources = sources
        .into_iter()
        .zip(physical)
        .map(|(source, target)| {
            source
                .with_retrieval_entries(target.entries)
                .with_recheck_pins(target.recheck_pins)
        })
        .collect::<Vec<_>>();
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
