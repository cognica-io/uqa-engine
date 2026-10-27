//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Only downstream handoffs pause a native producer's measurement.

use super::Step;
use crate::{relational::AggregateExecutor, Batch, ExecResult};
use std::cell::RefCell;

pub(super) struct AggregateReceiver<'a> {
    pub inner: &'a mut dyn AggregateExecutor,
    pub step: &'a mut Step,
}

impl AggregateExecutor for AggregateReceiver<'_> {
    fn consume(&mut self, batch: Batch) -> ExecResult<()> {
        self.step.ready(batch.rows.len())?;
        self.step.downstream(|| self.inner.consume(batch))
    }

    fn supports_projected_rows(&self) -> bool {
        self.inner.supports_projected_rows()
    }
    fn supports_storage_borrowed_rows(&self) -> bool {
        self.inner.supports_storage_borrowed_rows()
    }

    fn consume_projected_row(&mut self, row: &crate::ProjectedRow<'_, '_>) -> ExecResult<()> {
        self.step.ready(1)?;
        self.step
            .downstream(|| self.inner.consume_projected_row(row))
    }

    fn finish(&mut self) -> ExecResult<crate::spill::SpillBuffer> {
        self.step.downstream(|| self.inner.finish())
    }

    fn close(&mut self) -> ExecResult<()> {
        self.step.downstream(|| self.inner.close())
    }
}

/// This private chain can only observe the concrete, effect-free result sink. It does not expose callbacks under a storage borrow.
pub(crate) struct MaterializedObservation<'a> {
    pub(crate) step: &'a RefCell<Step>,
    pub(crate) parent: Option<&'a MaterializedObservation<'a>>,
}

impl MaterializedObservation<'_> {
    pub(crate) fn output<T>(&self, consume: impl FnOnce() -> ExecResult<T>) -> ExecResult<T> {
        let mut step = self.step.borrow_mut();
        step.ready(1)?;
        step.downstream(|| match self.parent {
            Some(parent) => parent.output(consume),
            None => consume(),
        })
    }
}
