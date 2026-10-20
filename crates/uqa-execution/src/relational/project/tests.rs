//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ExpressionEvaluator, PhysicalRow, TableScan};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use uqa_sql::{expr::RowLookup, SQLError};

struct ExpandingEvaluator {
    calls: Arc<AtomicUsize>,
    fail_at: Option<i64>,
}

impl ExpressionEvaluator for ExpandingEvaluator {
    fn evaluate(&self, _: &ScalarExpr, row: &dyn RowLookup) -> ExecResult<Value> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let Some(Value::Int(id)) = row.column("id") else {
            panic!("missing input identity");
        };
        if Some(*id) == self.fail_at {
            return Err(SQLError::Routine {
                sqlstate: "22012".into(),
                message: "division by zero".into(),
            }
            .into());
        }
        Ok(Value::Str("x".repeat(131_072)))
    }
}

fn projection(rows: i64, fail_at: Option<i64>) -> (Project<'static>, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let source = TableScan::from_physical_rows(
        RowSchema::new(vec!["id".into()]),
        (0..rows)
            .map(|id| PhysicalRow::from_values(vec![Value::Int(id)]))
            .collect(),
    );
    let evaluator = Arc::new(ExpandingEvaluator {
        calls: Arc::clone(&calls),
        fail_at,
    });
    (
        Project::with_evaluator(
            Box::new(source),
            vec![
                ("same".into(), ScalarExpr::Column("id".into())),
                (
                    "same".into(),
                    ScalarExpr::Literal(Value::Str(String::new())),
                ),
            ],
            evaluator,
        )
        .with_memory_budget(512 * 1024),
        calls,
    )
}

#[test]
fn expanded_projection_spills_before_retaining_a_complete_input_batch() {
    let (mut projection, calls) = projection(128, None);
    projection.open().unwrap();
    let mut seen = 0;
    let allocated = allocation_counter::measure(|| {
        while let Some(batch) = projection.next().unwrap() {
            // The original input batch finishes evaluation before any output becomes visible.
            assert_eq!(calls.load(Ordering::Relaxed), 128);
            assert_eq!(batch.schema.columns(), ["same", "same"]);
            assert!(batch.rows.len() < 128);
            for row in batch.rows {
                assert_eq!(row.value(0), Some(&Value::Int(seen)));
                let Some(Value::Str(value)) = row.value(1) else {
                    panic!("missing computed value");
                };
                assert_eq!(value.len(), 131_072);
                assert!(value.bytes().all(|byte| byte == b'x'));
                seen += 1;
            }
        }
    });
    assert!(allocated.bytes_max < 2 * 1024 * 1024, "{allocated:?}");
    assert_eq!(seen, 128);
    assert!(projection.pending.is_none());
    projection.close().unwrap();
}

#[test]
fn expanded_projection_reports_late_input_error_before_any_output() {
    let (mut projection, calls) = projection(128, Some(127));
    projection.open().unwrap();
    let Err(crate::ExecError::SQL(error)) = projection.next() else {
        panic!("the original batch error must precede all output");
    };
    assert_eq!(error.sqlstate(), Some("22012"));
    assert_eq!(calls.load(Ordering::Relaxed), 128);
    assert!(projection.pending.is_none());
    projection.close().unwrap();
}

#[test]
fn expanded_projection_close_discards_its_unread_spill() {
    let (mut projection, calls) = projection(128, None);
    projection.open().unwrap();
    let first = projection.next().unwrap().unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 128);
    assert!(first.rows.len() < 128);
    assert!(projection.pending.is_some());
    drop(first);
    projection.close().unwrap();
    assert!(projection.pending.is_none());
    assert!(projection.next().unwrap().is_none());
}

#[test]
fn expanded_projection_cancel_discards_its_unread_spill() {
    let token = uqa_core::CancellationToken::new();
    let _scope = uqa_sql::expr::StatementCancellationScope::enter(token.clone());
    let (mut projection, calls) = projection(128, None);
    projection.open().unwrap();
    drop(projection.next().unwrap().unwrap());
    token.cancel();
    let Err(crate::ExecError::SQL(error)) = projection.next() else {
        panic!("cancellation must stop the retained reader");
    };
    assert_eq!(error.sqlstate(), Some("57014"));
    assert_eq!(calls.load(Ordering::Relaxed), 128);
    assert!(projection.pending.is_none());
    projection.close().unwrap();
}

#[test]
fn single_row_projection_retains_its_original_unsplit_layout() {
    let (mut projection, calls) = projection(1, None);
    projection.work_mem = Some(0);
    projection.open().unwrap();
    let batch = projection.next().unwrap().unwrap();
    assert_eq!(batch.rows.len(), 1);
    assert_eq!(batch.rows[0].fragment_count(), 2);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert!(projection.pending.is_none());
    assert!(projection.next().unwrap().is_none());
    projection.close().unwrap();
}
