//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Captured transition images before a trigger observes the relation descriptor.

use uqa_core::{CancellationToken, Value};
use uqa_sql::error::Result;
use uqa_sql::SQLError;

use crate::physical::physical_exec_error;
use crate::spill::{SpillBatchWriter, SpillDrain, SpillRows};
use crate::{PhysicalRow, RowSchema, SpillBuffer};

#[derive(Default)]
pub(super) struct TransitionImages {
    pending: Option<ImageBuffer>,
}

struct ImageBuffer {
    rows: SpillBuffer,
    writer: SpillBatchWriter,
}

impl TransitionImages {
    pub(super) fn push(
        &mut self,
        value: &Value,
        work_mem: impl FnOnce() -> Result<usize>,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        cancellation.check()?;
        if self.pending.is_none() {
            self.pending = Some(ImageBuffer {
                rows: SpillBuffer::new(work_mem()?.max(1)),
                writer: SpillBatchWriter::new(RowSchema::new(vec!["image".into()]))
                    .map_err(physical_exec_error)?,
            });
        }
        let pending = self.pending.as_mut().expect("initialized image buffer");
        // Preserve the raw carrier; relation metadata is resolved only when the trigger reads it.
        pending
            .writer
            .push(
                &mut pending.rows,
                PhysicalRow::from_values(vec![value.clone()]),
            )
            .map_err(physical_exec_error)
    }

    pub(super) fn into_rows(self, cancellation: CancellationToken) -> Result<TransitionImageRows> {
        cancellation.check()?;
        let rows = self
            .pending
            .map(|mut pending| {
                pending
                    .writer
                    .finish(&mut pending.rows)
                    .map_err(physical_exec_error)?;
                pending.rows.drain_rows().map_err(physical_exec_error)
            })
            .transpose()?;
        Ok(TransitionImageRows {
            rows,
            cancellation,
            finished: false,
        })
    }
}

pub(super) struct TransitionImageRows {
    rows: Option<SpillRows<SpillDrain>>,
    cancellation: CancellationToken,
    finished: bool,
}

impl Iterator for TransitionImageRows {
    type Item = Result<Value>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        if let Err(error) = self.cancellation.check() {
            self.finished = true;
            return Some(Err(error.into()));
        }
        let result = self.rows.as_mut().and_then(Iterator::next).map(|row| {
            let row = row.map_err(physical_exec_error)?;
            let mut values = row.row.into_physical_values();
            if values.len() != 1 {
                return Err(SQLError::Internal(
                    "transition capture row must contain one image".into(),
                ));
            }
            Ok(values.pop().expect("single captured image"))
        });
        if result.as_ref().is_none_or(Result::is_err) {
            self.finished = true;
        }
        result
    }
}

#[cfg(test)]
mod tests;
