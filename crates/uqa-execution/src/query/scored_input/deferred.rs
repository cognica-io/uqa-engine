//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained row inputs execute once when their consumer first pulls.

use super::{RecheckDoc, ScoredDocumentSource, ScoredEntriesProducer, ScoredInput};
use crate::{Batch, ExecResult, PhysicalOperator, RowSchema, RowSource, TableScan};
use std::sync::Arc;
use uqa_sql::SQLError;

#[cfg(test)]
mod tests;

type SourceProducer<'a> = Box<dyn FnOnce() -> Result<Box<dyn RowSource>, SQLError> + Send + 'a>;

pub(in crate::query) struct DeferredTableScan<'a> {
    schema: RowSchema,
    pending: Option<SourceProducer<'a>>,
    inner: Option<TableScan>,
}

impl<'a> DeferredTableScan<'a> {
    pub(in crate::query) fn new(schema: RowSchema, pending: SourceProducer<'a>) -> Self {
        Self {
            schema,
            pending: Some(pending),
            inner: None,
        }
    }

    fn input(&mut self) -> ExecResult<&mut TableScan> {
        if self.inner.is_none() {
            let pending = self.pending.take().ok_or_else(|| {
                SQLError::Internal("deferred source was already closed or failed".into())
            })?;
            let source = TableScan::new(pending()?);
            if source.row_schema() != &self.schema {
                return Err(SQLError::Internal(
                    "deferred source changed its bound row schema".into(),
                )
                .into());
            }
            self.inner = Some(source);
            self.inner.as_mut().expect("initialized source").open()?;
        }
        Ok(self.inner.as_mut().expect("initialized source"))
    }
}

impl PhysicalOperator for DeferredTableScan<'_> {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }
    fn backward_scan_support(&self) -> crate::BackwardScanSupport {
        crate::BackwardScanSupport::Materialize
    }
    fn estimated_cardinality(&self) -> Option<u64> {
        self.inner
            .as_ref()
            .and_then(PhysicalOperator::estimated_cardinality)
    }
    fn open(&mut self) -> ExecResult<()> {
        if let Some(inner) = &mut self.inner {
            inner.open()?;
        }
        Ok(())
    }
    fn next(&mut self) -> ExecResult<Option<Batch>> {
        self.input()?.next()
    }
    fn consume_into_aggregate(
        &mut self,
        executor: &mut dyn crate::AggregateExecutor,
    ) -> ExecResult<bool> {
        self.input()?.consume_into_aggregate(executor)
    }
    fn close(&mut self) -> ExecResult<()> {
        self.pending = None;
        if let Some(inner) = &mut self.inner {
            inner.close()?;
        }
        Ok(())
    }
}

pub(in crate::query) fn defer_entries<'a>(
    source: ScoredDocumentSource,
    pending: Option<ScoredEntriesProducer<'a>>,
    top_k: Option<usize>,
    pins: Option<Arc<Vec<RecheckDoc>>>,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    let Some(pending) = pending else {
        return Ok(Box::new(TableScan::new(Box::new(
            source.with_recheck_pins(pins),
        ))));
    };
    let schema = source
        .physical_schema()
        .cloned()
        .ok_or_else(|| SQLError::Internal("scored retrieval has no bound row schema".into()))?;
    Ok(Box::new(DeferredTableScan::new(
        schema,
        Box::new(move || {
            let mut input = ScoredInput::entries(pending()?, true);
            if let Some(top_k) = top_k {
                input.retain_top_scores_with_ties(top_k);
            }
            let ScoredInput::Entries { entries, .. } = input else {
                unreachable!("explicit scored entries")
            };
            Ok(Box::new(
                source
                    .with_retrieval_entries(entries)
                    .with_recheck_pins(pins),
            ))
        }),
    )))
}
