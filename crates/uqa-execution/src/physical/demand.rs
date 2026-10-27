//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain already-produced input rows while propagating a consumer's next pull.

use crate::{Batch, ExecError, ExecResult, PhysicalOperator, PhysicalRow};
use std::num::NonZeroUsize;

pub(crate) const ONE: NonZeroUsize = NonZeroUsize::new(1).unwrap();

#[derive(Default)]
pub(crate) struct PendingRows {
    rows: std::vec::IntoIter<PhysicalRow>,
}

impl PendingRows {
    pub(crate) fn retain_batch(&mut self, batch: Batch) {
        debug_assert_eq!(self.rows.len(), 0);
        self.rows = batch.rows.into_iter();
    }

    pub(crate) fn clear(&mut self) {
        self.rows = Vec::new().into_iter();
    }

    /// Pull a candidate without allocating a one-row batch around already retained input. An unrestricted caller keeps ordinary child batches; a bounded caller propagates one newly evaluated candidate.
    pub(crate) fn next_row(
        &mut self,
        input: &mut dyn PhysicalOperator,
        demanded: bool,
    ) -> ExecResult<Option<PhysicalRow>> {
        loop {
            if let Some(row) = self.rows.next() {
                return Ok(Some(row));
            }
            let batch = if demanded {
                input.next_with_demand(ONE)?
            } else {
                input.next()?
            };
            let Some(batch) = batch else { return Ok(None) };
            if &batch.schema != input.row_schema() {
                return Err(ExecError::Other(
                    "demanded input changed its row schema".into(),
                ));
            }
            self.retain_batch(batch);
        }
    }

    /// A materialized child may return a larger batch, but no parent expression observes its unused rows until a later pull.
    pub(crate) fn pull(
        &mut self,
        input: &mut dyn PhysicalOperator,
        demand: Option<NonZeroUsize>,
    ) -> ExecResult<Option<Batch>> {
        loop {
            if self.rows.len() != 0 {
                let count = demand.map_or(self.rows.len(), NonZeroUsize::get);
                let rows = self.rows.by_ref().take(count).collect();
                return Ok(Some(Batch::from_physical_rows(
                    input.row_schema().clone(),
                    rows,
                )));
            }
            let batch = match demand {
                Some(rows) => input.next_with_demand(rows)?,
                None => input.next()?,
            };
            let Some(batch) = batch else { return Ok(None) };
            if &batch.schema != input.row_schema() {
                return Err(ExecError::Other(
                    "demanded input changed its row schema".into(),
                ));
            }
            if demand.is_none_or(|rows| batch.len() <= rows.get()) {
                return Ok(Some(batch));
            }
            self.rows = batch.rows.into_iter();
        }
    }
}

/// An owned, demand-driven physical cursor used by correlated sources. It runs its producer once and closes it on exhaustion, error, or early drop.
pub(crate) struct OperatorRows<'a> {
    input: Box<dyn PhysicalOperator + 'a>,
    pending: PendingRows,
    finished: bool,
}

impl<'a> OperatorRows<'a> {
    pub(crate) fn open(mut input: Box<dyn PhysicalOperator + 'a>) -> ExecResult<Self> {
        if let Err(error) = input.open() {
            return super::with_cleanup(Err(error), input.close(), "open correlated row source");
        }
        Ok(Self {
            input,
            pending: PendingRows::default(),
            finished: false,
        })
    }

    fn finish(&mut self) -> ExecResult<()> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        self.pending.clear();
        self.input.close()
    }
}

impl crate::SQLRowSource<crate::OwnedPhysicalRow> for OperatorRows<'_> {
    fn next_row(&mut self) -> Result<Option<crate::OwnedPhysicalRow>, uqa_sql::SQLError> {
        if self.finished {
            return Ok(None);
        }
        self.pending
            .next_row(self.input.as_mut(), true)
            .map(|row| {
                row.map(|row| crate::OwnedPhysicalRow::new(self.input.row_schema().clone(), row))
            })
            .map_err(super::physical_exec_error)
    }

    fn close(&mut self) -> Result<(), uqa_sql::SQLError> {
        self.finish().map_err(super::physical_exec_error)
    }
}

impl Drop for OperatorRows<'_> {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}
