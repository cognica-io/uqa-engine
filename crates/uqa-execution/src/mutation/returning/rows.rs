//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered RETURNING values detach from source fragments and spill in bounded batches.

use crate::{
    physical::physical_exec_error,
    query::{projection::physical_work_mem_bytes, runtime::QueryRuntimeView},
    spill::{SpillDrain, SpillRows},
    Batch, IndexedSpill, OwnedPhysicalRow, PhysicalRow, RowSchema, SpillBuffer,
};
use uqa_core::CancellationToken;
use uqa_sql::SQLError;

pub struct ReturningRows {
    buffer: SpillBuffer,
    schema: Option<RowSchema>,
    pending: Vec<PhysicalRow>,
    pending_bytes: usize,
    batch_bytes: usize,
    cancellation: CancellationToken,
}

impl Default for ReturningRows {
    fn default() -> Self {
        Self::with_budget(0, CancellationToken::new())
    }
}

impl ReturningRows {
    pub fn new(runtime: QueryRuntimeView<'_>) -> Result<Self, SQLError> {
        Ok(Self::with_budget(
            physical_work_mem_bytes(runtime)?,
            runtime.cancellation_token(),
        ))
    }

    pub(crate) fn with_budget(bytes: usize, cancellation: CancellationToken) -> Self {
        Self {
            buffer: SpillBuffer::new(bytes / 2),
            schema: None,
            pending: Vec::new(),
            pending_bytes: 0,
            batch_bytes: bytes / 2,
            cancellation,
        }
    }

    pub fn len(&self) -> usize {
        self.buffer.rows() + self.pending.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub(crate) fn schema(&self) -> Option<&RowSchema> {
        self.schema.as_ref()
    }

    pub fn push(&mut self, row: OwnedPhysicalRow) -> Result<(), SQLError> {
        self.cancellation.check()?;
        self.len()
            .checked_add(1)
            .ok_or_else(|| SQLError::Internal("RETURNING row count overflow".into()))?;
        let schema = self
            .schema
            .get_or_insert_with(|| RowSchema::new(row.schema.columns().to_vec()));
        if schema.columns() != row.schema.columns() {
            return Err(SQLError::Internal(
                "RETURNING output columns changed".into(),
            ));
        }
        // A projected row may share an entire OLD/NEW document. Keep only its output slots; the final result binder still owns declared types and collations.
        let projected = row.row.project_slots(row.schema.layout_slots());
        drop(row);
        let row = PhysicalRow::from_values(projected.into_physical_values());
        let bytes = IndexedSpill::encoded_row_size(schema, &row).map_err(physical_exec_error)?;
        if self
            .pending_bytes
            .checked_add(bytes)
            .is_none_or(|bytes| bytes > self.batch_bytes)
        {
            self.flush()?;
        }
        self.pending_bytes = bytes
            .checked_add(self.pending_bytes)
            .ok_or_else(|| SQLError::Internal("RETURNING batch byte count overflow".into()))?;
        self.pending.push(row);
        if self.pending_bytes >= self.batch_bytes
            || self.pending.len() == crate::batch::DEFAULT_BATCH_SIZE
        {
            self.flush()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), SQLError> {
        if self.pending.is_empty() {
            return Ok(());
        }
        self.cancellation.check()?;
        let schema = self
            .schema
            .as_ref()
            .expect("pending RETURNING rows have a schema");
        let batch = Batch::from_physical_rows(schema.clone(), std::mem::take(&mut self.pending));
        self.pending_bytes = 0;
        self.buffer.push(batch).map_err(physical_exec_error)?;
        Ok(())
    }

    pub(crate) fn into_batches(mut self) -> Result<ReturningBatchReader, SQLError> {
        self.flush()?;
        Ok(ReturningBatchReader {
            rows: self.buffer.drain().map_err(physical_exec_error)?,
            cancellation: self.cancellation,
            failed: false,
        })
    }

    pub fn into_rows(self) -> Result<ReturningRowReader, SQLError> {
        let cancellation = self.cancellation.clone();
        Ok(ReturningRowReader {
            rows: SpillRows::new(self.into_batches()?),
            cancellation,
            failed: false,
        })
    }
}

pub struct ReturningRowReader {
    rows: SpillRows<ReturningBatchReader>,
    cancellation: CancellationToken,
    failed: bool,
}

pub(crate) struct ReturningBatchReader {
    rows: SpillDrain,
    cancellation: CancellationToken,
    failed: bool,
}

impl Iterator for ReturningBatchReader {
    type Item = crate::physical::ExecResult<Batch>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        let result = self
            .cancellation
            .check()
            .map_err(SQLError::from)
            .map_err(crate::physical::ExecError::from)
            .and_then(|()| self.rows.next().transpose());
        self.failed = result.is_err();
        result.transpose()
    }
}

impl Iterator for ReturningRowReader {
    type Item = Result<OwnedPhysicalRow, SQLError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        let result = self
            .cancellation
            .check()
            .map_err(SQLError::from)
            .and_then(|()| self.rows.next().transpose().map_err(physical_exec_error));
        self.failed = result.is_err();
        result.transpose()
    }
}

#[cfg(test)]
mod tests;
