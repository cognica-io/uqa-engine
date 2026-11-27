//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded output for joins that resume their probe after each pull.

use crate::spill::{SpillBatchWriter, SpillDrain};
use crate::{Batch, ExecResult, PhysicalRow, RowSchema};

#[derive(Default)]
pub(super) struct JoinOutput {
    pub(super) remaining: Option<SpillDrain>,
    pub(super) spilled: bool,
}

impl JoinOutput {
    pub(super) fn next_pending(&mut self) -> ExecResult<Option<Batch>> {
        self.remaining.as_mut().and_then(Iterator::next).transpose()
    }

    pub(super) fn drain_join_batch(
        &mut self,
        schema: RowSchema,
        work_mem_bytes: usize,
        rows: usize,
        mut next: impl FnMut() -> ExecResult<Option<PhysicalRow>>,
    ) -> ExecResult<Option<Batch>> {
        if let Some(batch) = self.next_pending()? {
            return Ok(Some(batch));
        }
        self.remaining = None;
        let budget = work_mem_bytes - work_mem_bytes / 2;
        let mut writer = SpillBatchWriter::new(schema)?;
        let mut spill = None;
        debug_assert!(rows <= crate::batch::DEFAULT_BATCH_SIZE);
        for _ in 0..rows {
            let Some(row) = next()? else { break };
            writer.push_or_spill(row, budget, &mut spill)?;
        }
        let Some(mut spill) = spill else {
            return Ok(writer.into_batch());
        };
        writer.finish(&mut spill)?;
        self.spilled |= spill.has_spilled();
        self.remaining = Some(spill.drain()?);
        self.next_pending()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::Value;

    #[test]
    fn join_output_moves_resident_rows_and_drains_spilled_rows_before_polling_again() {
        let schema = RowSchema::new(vec!["v".into()]);
        let payload = "row".repeat(1024);
        let address = payload.as_ptr();
        let mut rows = [PhysicalRow::from_values(vec![Value::Str(payload)])].into_iter();
        let mut output = JoinOutput::default();
        let batch = output
            .drain_join_batch(schema.clone(), usize::MAX, 2, || Ok(rows.next()))
            .unwrap()
            .unwrap();
        let Some(Value::Str(value)) = batch.rows[0].value(0) else {
            panic!("missing string output");
        };
        assert_eq!(value.as_ptr(), address);
        assert!(!output.spilled && output.remaining.is_none());

        let mut rows = (0..16).map(|i| PhysicalRow::from_values(vec![Value::Int(i)]));
        let first = output
            .drain_join_batch(schema.clone(), 1, 16, || Ok(rows.next()))
            .unwrap()
            .unwrap();
        assert!(output.spilled);
        assert_eq!(first.rows.len(), 1);
        assert_eq!(first.rows[0].value(0), Some(&Value::Int(0)));
        for expected in 1..16 {
            let batch = output
                .drain_join_batch(schema.clone(), 1, 1, || panic!("polled past retained rows"))
                .unwrap()
                .unwrap();
            assert_eq!(batch.rows.len(), 1);
            assert_eq!(batch.rows[0].value(0), Some(&Value::Int(expected)));
        }
        assert!(output
            .drain_join_batch(schema.clone(), 1, 1, || Ok(rows.next()))
            .unwrap()
            .is_none());
        for budget in [1, usize::MAX] {
            let error = output
                .drain_join_batch(schema.clone(), budget, 1, || {
                    Ok(Some(PhysicalRow::from_values(vec![])))
                })
                .unwrap_err();
            assert!(error
                .to_string()
                .contains("physical row is missing slot 0 of 1"));
        }
    }
}
