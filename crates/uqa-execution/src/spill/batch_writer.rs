//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Append positional rows in byte- and row-bounded batches while preserving their complete schema.

use super::{EncodedBatchSizer, SpillBuffer};
use crate::{Batch, ExecResult, PhysicalRow, RowSchema};

pub(crate) struct SpillBatchWriter {
    schema: RowSchema,
    pending: Vec<PhysicalRow>,
    pending_size: EncodedBatchSizer,
}

impl SpillBatchWriter {
    pub(crate) fn new(schema: RowSchema) -> ExecResult<Self> {
        let pending_size = EncodedBatchSizer::new(&schema)?;
        Ok(Self {
            schema,
            pending: Vec::new(),
            pending_size,
        })
    }

    pub(crate) fn push(&mut self, output: &mut SpillBuffer, record: PhysicalRow) -> ExecResult<()> {
        let mut candidate_size = self.pending_size;
        candidate_size.append(&record)?;
        let exceeds_budget = candidate_size.bytes() > output.budget_bytes();
        if exceeds_budget && !self.pending.is_empty() {
            self.flush(output)?;
            candidate_size = self.pending_size;
            candidate_size.append(&record)?;
        }
        self.pending.push(record);
        self.pending_size = candidate_size;
        if self.pending_size.bytes() > output.budget_bytes()
            || self.pending.len() == crate::batch::DEFAULT_BATCH_SIZE
        {
            self.flush(output)?;
        }
        Ok(())
    }

    pub(crate) fn finish(mut self, output: &mut SpillBuffer) -> ExecResult<()> {
        self.flush(output)
    }

    fn flush(&mut self, output: &mut SpillBuffer) -> ExecResult<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        output.push(Batch::from_physical_rows(
            self.schema.clone(),
            std::mem::take(&mut self.pending),
        ))?;
        self.pending_size = EncodedBatchSizer::new(&self.schema)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spill_batch_writer_bounds_zero_width_row_batches_and_keeps_their_count() {
        let schema = RowSchema::default();
        let mut output = SpillBuffer::new(64 * 1024);
        let mut writer = SpillBatchWriter::new(schema.clone()).unwrap();
        for _ in 0..5000 {
            writer
                .push(&mut output, PhysicalRow::from_values(Vec::new()))
                .unwrap();
        }
        writer.finish(&mut output).unwrap();
        let rows = output.into_shared(schema).unwrap();
        let reader = rows.reader().unwrap();
        let mut count = 0;
        for batch in reader {
            let batch = batch.unwrap();
            assert!(batch.rows.len() <= crate::batch::DEFAULT_BATCH_SIZE);
            assert_eq!(batch.schema.physical_width(), 0);
            count += batch.rows.len();
        }
        assert_eq!(count, 5000);
    }
}
