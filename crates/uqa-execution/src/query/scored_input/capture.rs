//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Preserve complete retrieval scores in bounded, ordered spill batches.

use crate::{
    physical::physical_exec_error,
    spill::{SharedSpillReader, SpillBatchWriter, SpillRows},
    PhysicalRow, RowSchema, SpillBuffer,
};
use uqa_core::{CancellationToken, ScoredEntry, Value};
use uqa_sql::SQLError;

pub(crate) struct ScoredEntryCapture {
    rows: SpillBuffer,
    writer: SpillBatchWriter,
    schema: RowSchema,
    cancellation: CancellationToken,
}

impl ScoredEntryCapture {
    pub(crate) fn new(bytes: usize, cancellation: CancellationToken) -> Result<Self, SQLError> {
        cancellation.check()?;
        let schema = RowSchema::with_internal_relation_types(
            uqa_sql::ast::InternalRelationId::allocate(),
            vec![None, None],
        );
        Ok(Self {
            rows: SpillBuffer::new(bytes / 4),
            writer: SpillBatchWriter::new(schema.clone()).map_err(physical_exec_error)?,
            schema,
            cancellation,
        })
    }

    pub(crate) fn push(&mut self, entry: ScoredEntry) -> Result<(), SQLError> {
        self.cancellation.check()?;
        uqa_sql::expr::check_statement_cancellation()?;
        self.writer
            .push(
                &mut self.rows,
                PhysicalRow::from_values(vec![
                    Value::Bytes(entry.doc_id.to_be_bytes().to_vec()),
                    Value::Float(entry.score),
                ]),
            )
            .map_err(physical_exec_error)
    }

    pub(crate) fn finish(mut self) -> Result<CapturedScoredEntries, SQLError> {
        self.cancellation.check()?;
        self.writer
            .finish(&mut self.rows)
            .map_err(physical_exec_error)?;
        let remaining = self.rows.rows();
        let rows = self
            .rows
            .into_shared(self.schema)
            .map_err(physical_exec_error)?;
        Ok(CapturedScoredEntries {
            rows: Some(rows.read_rows().map_err(physical_exec_error)?),
            remaining,
            cancellation: self.cancellation,
        })
    }
}

/// Completed retrieval input. Its original production order, document identities and score bits survive spill; errors and cancellation release its unread records.
pub struct CapturedScoredEntries {
    rows: Option<SpillRows<SharedSpillReader>>,
    remaining: usize,
    cancellation: CancellationToken,
}

impl CapturedScoredEntries {
    fn read(&mut self) -> Result<ScoredEntry, SQLError> {
        self.cancellation.check()?;
        uqa_sql::expr::check_statement_cancellation()?;
        let record = self
            .rows
            .as_mut()
            .and_then(Iterator::next)
            .ok_or_else(|| SQLError::Internal("truncated captured retrieval input".into()))?
            .map_err(physical_exec_error)?;
        let (Some(Value::Bytes(identity)), Some(Value::Float(score))) =
            (record.row.value(0), record.row.value(1))
        else {
            return Err(SQLError::Internal(
                "invalid captured retrieval entry".into(),
            ));
        };
        let identity = identity.as_slice().try_into().map_err(|_| {
            SQLError::Internal("invalid captured retrieval document identity".into())
        })?;
        Ok(ScoredEntry {
            doc_id: u64::from_be_bytes(identity),
            score: *score,
        })
    }
}

impl Iterator for CapturedScoredEntries {
    type Item = Result<ScoredEntry, SQLError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            self.rows = None;
            return None;
        }
        let result = self.read();
        if result.is_ok() {
            self.remaining -= 1;
        } else {
            self.remaining = 0;
            self.rows = None;
        }
        Some(result)
    }
}

#[cfg(test)]
mod tests;
