//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Append positional rows in byte- and row-bounded batches while preserving their complete schema.

use super::{EncodedBatchSizer, SpillBuffer};
use crate::{ExecResult, PhysicalRow, RowSchema};

pub(crate) struct SpillBatchWriter {
    schema: RowSchema,
    pending: Vec<PhysicalRow>,
    pending_size: EncodedBatchSizer,
    empty_size: EncodedBatchSizer,
}

impl SpillBatchWriter {
    pub(crate) fn new(schema: RowSchema) -> ExecResult<Self> {
        let pending_size = EncodedBatchSizer::new(&schema)?;
        Ok(Self {
            schema,
            pending: Vec::new(),
            pending_size,
            empty_size: pending_size,
        })
    }

    pub(crate) fn push(&mut self, output: &mut SpillBuffer, record: PhysicalRow) -> ExecResult<()> {
        self.append(record, output.budget_bytes(), |writer| writer.flush(output))?;
        if self.pending.len() == crate::batch::DEFAULT_BATCH_SIZE {
            self.flush(output)?;
        }
        Ok(())
    }

    /// Collect one output batch directly. Allocate a spill buffer only when its rows exceed the byte allowance.
    pub(crate) fn push_or_spill(
        &mut self,
        record: PhysicalRow,
        budget: usize,
        spill: &mut Option<SpillBuffer>,
    ) -> ExecResult<()> {
        self.append(record, budget, |writer| {
            writer.flush(spill.get_or_insert_with(|| SpillBuffer::new(budget)))
        })
    }

    fn append(
        &mut self,
        record: PhysicalRow,
        budget: usize,
        mut flush: impl FnMut(&mut Self) -> ExecResult<()>,
    ) -> ExecResult<()> {
        let mut candidate = self.pending_size;
        candidate.append(&record)?;
        if candidate.bytes() > budget && !self.pending.is_empty() {
            flush(self)?;
            candidate = self.pending_size;
            candidate.append(&record)?;
        }
        self.pending.push(record);
        self.pending_size = candidate;
        if candidate.bytes() > budget {
            flush(self)?;
        }
        Ok(())
    }

    pub(crate) fn into_batch(self) -> Option<crate::Batch> {
        (!self.pending.is_empty())
            .then(|| crate::Batch::from_physical_rows(self.schema, self.pending))
    }

    pub(crate) fn finish(mut self, output: &mut SpillBuffer) -> ExecResult<()> {
        self.flush(output)
    }

    fn flush(&mut self, output: &mut SpillBuffer) -> ExecResult<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let size = std::mem::replace(&mut self.pending_size, self.empty_size);
        size.finish_rows(std::mem::take(&mut self.pending))
            .push_to(output, &self.schema)?;
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
