//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rows waiting for a portal reader share its invocation's memory allowance.

use crate::OwnedPhysicalRow;
use std::num::NonZeroUsize;
use uqa_core::{
    memory::{BudgetedVec, MemoryError, MemoryReservation, ProductionControl, ProductionVec},
    CancellationToken, Value, ValueRetentionError,
};
use uqa_sql::SQLError;
use uqa_storage::read_control::StorageReadControl;

pub struct WorkerBatch<R> {
    rows: Vec<R>,
    last: Option<R>,
    pub processed: usize,
    pub exhausted: bool,
    _memory: Option<MemoryReservation>,
}

impl<R> WorkerBatch<R> {
    pub fn exhausted() -> Self {
        Self {
            rows: Vec::new(),
            last: None,
            processed: 0,
            exhausted: true,
            _memory: None,
        }
    }

    pub fn append_to(mut self, destination: &mut Vec<R>) {
        if destination.is_empty() {
            std::mem::swap(destination, &mut self.rows);
        } else {
            destination.append(&mut self.rows);
        }
        destination.extend(self.last);
    }

    /// Deliver the rows while the worker's reservation still covers the buffered copies.
    pub fn visit(self, mut accept: impl FnMut(R) -> Result<(), SQLError>) -> Result<(), SQLError> {
        for row in self.rows.into_iter().chain(self.last) {
            accept(row)?;
        }
        Ok(())
    }
}

enum Buffered<R> {
    Ordinary(Vec<R>),
    Controlled {
        rows: BudgetedVec<R>,
        values: MemoryReservation,
    },
}

pub(super) struct PendingBatch<R> {
    buffered: Buffered<R>,
    cancellation: Option<CancellationToken>,
    limit: usize,
    pub(super) capture: bool,
    processed: usize,
}

pub(super) fn project<R>(row: &OwnedPhysicalRow, width: usize, output: fn(Vec<Value>) -> R) -> R {
    let view = row.view();
    output(
        (0..width)
            .map(|i| view.value_at(i).cloned().unwrap_or(Value::Null))
            .collect(),
    )
}

impl<R> PendingBatch<R> {
    pub(super) fn new(
        limit: NonZeroUsize,
        capture: bool,
        scope: Option<&StorageReadControl>,
    ) -> Self {
        let limit = limit.get().min(crate::DEFAULT_BATCH_SIZE);
        let cancellation = scope.map(|scope| scope.cancellation().clone());
        let buffered = match scope {
            Some(scope) if capture && limit > 1 => {
                // Buffering is optional. Leave room for the producer's next row and yield a short chunk whenever this allowance fills.
                let budget = scope
                    .memory()
                    .child((scope.memory().available() / 8).min(64 << 10));
                Buffered::Controlled {
                    rows: BudgetedVec::new(&budget),
                    values: budget.empty_reservation(),
                }
            }
            _ => Buffered::Ordinary(Vec::new()),
        };
        Self {
            buffered,
            cancellation,
            limit,
            capture,
            processed: 0,
        }
    }

    /// The current output row can always be handed straight to the reader, just as for a single step. If retaining it alongside earlier rows would exhaust the workspace, complete this chunk at the current row before evaluating another.
    pub(super) fn push(
        &mut self,
        row: &OwnedPhysicalRow,
        width: usize,
        output: fn(Vec<Value>) -> R,
        projection_bytes: usize,
    ) -> Result<bool, SQLError> {
        self.processed += 1;
        if let Some(cancellation) = &self.cancellation {
            cancellation.check()?;
        }
        if self.processed == self.limit {
            return Ok(true);
        }
        if !self.capture {
            return Ok(false);
        }
        match &mut self.buffered {
            Buffered::Ordinary(rows) => {
                rows.try_reserve(1).map_err(MemoryError::from)?;
                rows.push(project(row, width, output));
                Ok(false)
            }
            Buffered::Controlled { rows, values } => {
                let cancellation = self
                    .cancellation
                    .as_ref()
                    .expect("a controlled row batch owns its cancellation");
                let budget = rows.budget().clone();
                let control = ProductionControl::new(&budget, cancellation, cancellation);
                let copied = (|| {
                    rows.reserve(1)?;
                    let extra = control.reserve(projection_bytes)?;
                    let mut result = ProductionVec::new(control);
                    result.reserve(width)?;
                    let view = row.view();
                    for index in 0..width {
                        result.push_produced(
                            control.copy_value(view.value_at(index).unwrap_or(&Value::Null))?,
                        )?;
                    }
                    let (row, memory) = result.finish()?.into_parts();
                    control.finish(row, control.combine(memory, extra))
                })();
                match copied {
                    Ok(copied) => {
                        let (row, lease) = copied.into_parts();
                        if let Some(lease) = lease {
                            values.absorb(lease);
                        }
                        rows.push(output(row))?;
                        Ok(false)
                    }
                    Err(ValueRetentionError::Memory(MemoryError::Limit { .. })) => Ok(true),
                    Err(error) => Err(error.into()),
                }
            }
        }
    }

    pub(super) fn finish(self, last: Option<R>, exhausted: bool) -> WorkerBatch<R> {
        let (rows, memory) = match self.buffered {
            Buffered::Ordinary(rows) => (rows, None),
            Buffered::Controlled { rows, values, .. } => {
                let (rows, mut memory) = rows.into_parts();
                memory.absorb(values);
                (rows, Some(memory))
            }
        };
        WorkerBatch {
            rows,
            last,
            processed: self.processed,
            exhausted,
            _memory: memory,
        }
    }
}
