//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bound set-operation output independently of input row count and multiplicity.

use super::*;

struct WideRows {
    schema: RowSchema,
    remaining: usize,
}

impl PhysicalOperator for WideRows {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }
    fn open(&mut self) -> ExecResult<()> {
        Ok(())
    }
    fn next(&mut self) -> ExecResult<Option<Batch>> {
        if self.remaining == 0 {
            return Ok(None);
        }
        self.remaining -= 1;
        Ok(Some(Batch::from_physical_rows(
            self.schema.clone(),
            vec![PhysicalRow::from_values(vec![Value::Str(
                "x".repeat(32768),
            )])],
        )))
    }
    fn close(&mut self) -> ExecResult<()> {
        Ok(())
    }
}

#[test]
fn set_operation_output_batches_bound_wide_rows_and_repeated_groups() {
    for (kind, all, left, right, expected) in [
        (SetOpKind::Union, true, 512, 256, 768),
        (SetOpKind::Union, false, 512, 256, 1),
        (SetOpKind::Intersect, true, 512, 256, 256),
        (SetOpKind::Except, true, 512, 256, 256),
    ] {
        let scan = |remaining| {
            Box::new(WideRows {
                schema: RowSchema::with_types(vec!["v".into()], vec![Some(ColumnType::Text)]),
                remaining,
            })
        };
        let allocation = allocation_counter::measure(|| {
            let mut operation =
                ExternalSetOperation::new(scan(0), scan(0), kind, all, 64 * 1024).unwrap();
            // These identical rows already have the order produced by external sorting. Exercise the merge/output owner at full multiplicity without repeating the separate sort-run qualification.
            operation.left = RowCursor::new(scan(left));
            operation.right = RowCursor::new(scan(right));
            operation.open().unwrap();
            let mut count = 0;
            while let Some(batch) = operation.next().unwrap() {
                assert!(
                    batch.rows.len() <= 2,
                    "{kind:?} output retained a wide batch"
                );
                for row in &batch.rows {
                    let value = batch.schema.view(row).value_at(0).unwrap();
                    assert!(
                        matches!(value, Value::Str(text) if text.len() == 32768 && text.bytes().all(|byte| byte == b'x'))
                    );
                }
                count += batch.rows.len();
            }
            assert_eq!(count, expected, "{kind:?}, ALL={all}");
            operation.close().unwrap();
        });
        assert!(
            allocation.bytes_max < 2 * 1024 * 1024,
            "{kind:?}, ALL={all}: {allocation:?}"
        );
    }
}
