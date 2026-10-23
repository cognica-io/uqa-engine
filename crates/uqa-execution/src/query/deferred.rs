//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical sources begin execution only when their consumer first pulls.

use crate::{Batch, ExecResult, PhysicalOperator, RowSchema, RowSource, TableScan};
use uqa_sql::SQLError;

#[cfg(test)]
mod tests;

type OperatorProducer<'a> =
    Box<dyn FnOnce() -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> + Send + 'a>;

pub(in crate::query) struct DeferredOperator<'a> {
    schema: RowSchema,
    pending: Option<OperatorProducer<'a>>,
    inner: Option<Box<dyn PhysicalOperator + 'a>>,
    failed: bool,
}

impl<'a> DeferredOperator<'a> {
    /// A complete physical descriptor can defer production until the first pull. Open public columns or execution-owned metadata need the producer's descriptor before consumers bind names, wildcards and score sources.
    pub(in crate::query) fn from_bound_schema(
        schema: Option<RowSchema>,
        pending: OperatorProducer<'a>,
    ) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
        match schema {
            Some(schema) if !schema.columns_are_open(None) => {
                Ok(Box::new(Self::new(schema, pending)))
            }
            _ => pending(),
        }
    }

    pub(in crate::query) fn new(schema: RowSchema, pending: OperatorProducer<'a>) -> Self {
        Self {
            schema,
            pending: Some(pending),
            inner: None,
            failed: false,
        }
    }

    pub(in crate::query) fn from_rows(
        schema: RowSchema,
        pending: Box<dyn FnOnce() -> Result<Box<dyn RowSource>, SQLError> + Send + 'a>,
    ) -> Self {
        Self::new(
            schema,
            Box::new(move || Ok(Box::new(TableScan::new(pending()?)))),
        )
    }

    fn input(&mut self) -> ExecResult<&mut Box<dyn PhysicalOperator + 'a>> {
        if self.failed {
            return Err(
                SQLError::Internal("deferred source initialization already failed".into()).into(),
            );
        }
        if self.inner.is_none() {
            self.failed = true;
            let pending = self.pending.take().ok_or_else(|| {
                SQLError::Internal("deferred source was already closed or failed".into())
            })?;
            let source = pending()?;
            if source.row_schema() != &self.schema {
                return Err(SQLError::Internal(
                    "deferred source changed its bound row schema".into(),
                )
                .into());
            }
            self.inner = Some(source);
            self.inner.as_mut().expect("initialized source").open()?;
            self.failed = false;
        }
        Ok(self.inner.as_mut().expect("initialized source"))
    }
}

impl PhysicalOperator for DeferredOperator<'_> {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }
    fn backward_scan_support(&self) -> crate::BackwardScanSupport {
        crate::BackwardScanSupport::Materialize
    }
    fn estimated_cardinality(&self) -> Option<u64> {
        self.inner
            .as_ref()
            .and_then(|inner| inner.estimated_cardinality())
    }
    fn open(&mut self) -> ExecResult<()> {
        if let Some(inner) = &mut self.inner {
            self.failed = true;
            inner.open()?;
            self.failed = false;
        }
        Ok(())
    }
    fn next(&mut self) -> ExecResult<Option<Batch>> {
        self.input()?.next()
    }
    fn next_with_demand(&mut self, rows: std::num::NonZeroUsize) -> ExecResult<Option<Batch>> {
        self.input()?.next_with_demand(rows)
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
