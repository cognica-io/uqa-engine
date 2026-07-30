//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Volcano scan over an owned [`crate::spill::SpillBuffer`].
//!
//! The scan transfers ownership of the spill file to its row iterator at
//! `open`, so disk batches are decoded one at a time and the file is removed
//! even when execution stops early.

use crate::batch::{Batch, RowSchema, DEFAULT_BATCH_SIZE};
use crate::physical::{ExecResult, PhysicalOperator};
use crate::spill::{SharedSpill, SharedSpillReader, SpillBuffer, SpillDrain, SpillRows};

/// One-shot physical scan over a disk-backed spill buffer.
pub struct SpillScan {
    schema: RowSchema,
    buffer: Option<SpillBuffer>,
    rows: Option<SpillRows<SpillDrain>>,
}

/// Repeatable scan over an immutable shared spill. Cloning the source only
/// clones an `Arc`; `open` creates an independent file reader.
pub struct SharedSpillScan {
    source: SharedSpill,
    schema: RowSchema,
    reader: Option<SharedSpillReader>,
}

impl SharedSpillScan {
    pub fn new(source: SharedSpill) -> Self {
        let schema = RowSchema::new(source.schema().to_vec());
        Self {
            source,
            schema,
            reader: None,
        }
    }
}

impl PhysicalOperator for SharedSpillScan {
    fn schema(&self) -> &[String] {
        &self.schema.columns
    }

    fn open(&mut self) -> ExecResult<()> {
        self.reader = Some(self.source.reader()?);
        Ok(())
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        self.reader
            .as_mut()
            .map_or(Ok(None), |reader| reader.next().transpose())
    }

    fn close(&mut self) -> ExecResult<()> {
        self.reader = None;
        Ok(())
    }
}

impl SpillScan {
    pub fn new(schema: Vec<String>, buffer: SpillBuffer) -> Self {
        Self {
            schema: RowSchema::new(schema),
            buffer: Some(buffer),
            rows: None,
        }
    }
}

impl PhysicalOperator for SpillScan {
    fn schema(&self) -> &[String] {
        &self.schema.columns
    }

    fn open(&mut self) -> ExecResult<()> {
        let mut buffer = self.buffer.take().ok_or_else(|| {
            crate::physical::ExecError::Other("spill scan cannot be reopened".into())
        })?;
        self.rows = Some(buffer.drain_rows()?);
        Ok(())
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        let Some(rows) = self.rows.as_mut() else {
            return Ok(None);
        };
        let mut batch = Vec::with_capacity(DEFAULT_BATCH_SIZE);
        for _ in 0..DEFAULT_BATCH_SIZE {
            match rows.next() {
                Some(Ok(row)) => batch.push(row),
                Some(Err(error)) => return Err(error),
                None => break,
            }
        }
        if batch.is_empty() {
            Ok(None)
        } else {
            Ok(Some(Batch::new(self.schema.clone(), batch)))
        }
    }

    fn close(&mut self) -> ExecResult<()> {
        self.rows = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use uqa_core::Value;

    use super::*;
    use crate::physical::run_to_rows;

    #[test]
    fn scan_streams_a_forced_spill_in_input_order() {
        let schema = RowSchema::new(vec!["x".into()]);
        let mut spill = SpillBuffer::new(1);
        for value in 0..300_i64 {
            spill
                .push(Batch::new(
                    schema.clone(),
                    vec![BTreeMap::from([("x".into(), Value::Int(value))])],
                ))
                .unwrap();
        }
        assert!(spill.has_spilled());

        let mut scan = SpillScan::new(schema.columns, spill);
        let (_, rows) = run_to_rows(&mut scan).unwrap();
        assert_eq!(rows.len(), 300);
        for (expected, row) in rows.iter().enumerate() {
            assert_eq!(row.get("x"), Some(&Value::Int(expected as i64)));
        }
    }

    #[test]
    fn shared_spill_supports_independent_repeatable_scans() {
        let schema = RowSchema::new(vec!["x".into()]);
        let mut spill = SpillBuffer::new(1);
        for value in 0..2_048_i64 {
            spill
                .push(Batch::new(
                    schema.clone(),
                    vec![BTreeMap::from([("x".into(), Value::Int(value))])],
                ))
                .unwrap();
        }
        let shared = spill.into_shared(schema.columns).unwrap();
        let mut first = SharedSpillScan::new(shared.clone());
        let mut second = SharedSpillScan::new(shared);
        let (_, first_rows) = run_to_rows(&mut first).unwrap();
        let (_, second_rows) = run_to_rows(&mut second).unwrap();
        assert_eq!(first_rows, second_rows);
        assert_eq!(first_rows.len(), 2_048);
    }
}
