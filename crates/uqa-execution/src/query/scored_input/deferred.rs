//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Defer scored row production until the consumer requests rows.

use super::{
    ProducedScoredEntries, RecheckDoc, ScoredDocumentSource, ScoredEntriesProducer, ScoredInput,
};
use crate::query::deferred::DeferredOperator;
use crate::{PhysicalOperator, RowSource, TableScan};
use std::sync::Arc;
use uqa_sql::SQLError;

pub(in crate::query) fn defer_entries<'a>(
    source: ScoredDocumentSource,
    pending: Option<ScoredEntriesProducer<'a>>,
    top_k: Option<usize>,
    pins: Option<Arc<Vec<RecheckDoc>>>,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    let Some(pending) = pending else {
        return Ok(Box::new(TableScan::new(Box::new(
            source.with_recheck_pins(pins)?,
        ))));
    };
    let schema = source
        .physical_schema()
        .cloned()
        .ok_or_else(|| SQLError::Internal("scored retrieval has no bound row schema".into()))?;
    Ok(Box::new(DeferredOperator::from_rows(
        schema,
        Box::new(move || {
            let entries = match pending()? {
                ProducedScoredEntries::Materialized(entries) => entries,
                ProducedScoredEntries::Captured(entries) => {
                    if top_k.is_some() {
                        return Err(SQLError::Internal(
                            "complete retrieval capture cannot apply a score cutoff".into(),
                        ));
                    }
                    return Ok(Box::new(
                        source
                            .with_captured_entries(entries)
                            .with_recheck_pins(pins)?,
                    ) as Box<dyn RowSource>);
                }
            };
            let mut input = ScoredInput::entries(entries, true);
            if let Some(top_k) = top_k {
                input.retain_top_scores_with_ties(top_k);
            }
            let ScoredInput::Entries { entries, .. } = input else {
                unreachable!("explicit scored entries")
            };
            Ok(Box::new(
                source
                    .with_retrieval_entries(entries)
                    .with_recheck_pins(pins)?,
            ))
        }),
    )))
}
