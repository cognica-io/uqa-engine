//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use uqa_core::{memory::MemoryBudget, CancellationToken, Value};

struct Input {
    schema: RowSchema,
    closes: Arc<AtomicUsize>,
    fail_close: bool,
}
impl PhysicalOperator for Input {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }
    fn open(&mut self) -> ExecResult<()> {
        Ok(())
    }
    fn next(&mut self) -> ExecResult<Option<Batch>> {
        panic!("construction must not pull rows")
    }
    fn close(&mut self) -> ExecResult<()> {
        self.closes.fetch_add(1, Ordering::SeqCst);
        if self.fail_close {
            Err(SQLError::Routine {
                sqlstate: "58030".into(),
                message: "filter cleanup failed".into(),
            }
            .into())
        } else {
            Ok(())
        }
    }
}
fn input(closes: &Arc<AtomicUsize>, fail_close: bool) -> Box<dyn PhysicalOperator> {
    Box::new(Input {
        schema: RowSchema::default(),
        closes: closes.clone(),
        fail_close,
    })
}

#[test]
fn owned_filter_construction_releases_cancelled_inputs_and_keeps_cleanup_diagnostics() {
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let memory = MemoryBudget::new(1 << 20);
    let control = ProductionControl::new(&memory, &original, &invoking);
    let evaluator = DefaultExpressionEvaluator::shared(Vec::new());
    for token in [&original, &invoking] {
        let predicate = ScalarExpr::Literal(Value::Bool(true))
            .copy_for_rewrite(&control)
            .unwrap();
        let closes = Arc::new(AtomicUsize::new(0));
        let child = input(&closes, true);
        token.cancel();
        let error = Filter::with_owned_predicate(child, predicate, evaluator.clone(), &control)
            .err()
            .unwrap();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert!(error.detail().unwrap().contains("filter cleanup failed"));
        assert_eq!(closes.load(Ordering::SeqCst), 1);
        assert_eq!(memory.used(), 0);
        token.reset();
    }
    let closes = Arc::new(AtomicUsize::new(0));
    let predicate = ScalarExpr::Literal(Value::Bool(true))
        .copy_for_rewrite(&control)
        .unwrap();
    let filter =
        Filter::with_owned_predicate(input(&closes, true), predicate, evaluator, &control).unwrap();
    let occupied = memory.reserve(memory.available()).unwrap();
    let error = filter.into_boxed(&control).err().unwrap();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert!(error.detail().unwrap().contains("filter cleanup failed"));
    assert_eq!(closes.load(Ordering::SeqCst), 1);
    assert_eq!(memory.used(), occupied.bytes());
    drop(occupied);
    assert_eq!(memory.used(), 0);
}

#[test]
fn owned_filter_retains_exact_storage_and_preserves_deferred_column_errors() {
    let memory = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    let evaluator = DefaultExpressionEvaluator::shared(Vec::new());
    let closes = Arc::new(AtomicUsize::new(0));
    let child = input(&closes, false);
    let mut output = None;
    let allocation = allocation_counter::measure(|| {
        let predicate = ScalarExpr::Literal(Value::Bool(true))
            .copy_for_rewrite(&control)
            .unwrap();
        output = Some(
            Filter::with_owned_predicate(child, predicate, evaluator, &control)
                .unwrap()
                .into_boxed(&control)
                .unwrap(),
        );
    });
    assert_eq!(allocation.bytes_current, memory.used() as i64);
    assert!(memory.used() >= size_of::<Filter<'_>>());
    output.as_mut().unwrap().close().unwrap();
    drop(output);
    assert_eq!(closes.load(Ordering::SeqCst), 1);
    assert_eq!(memory.used(), 0);
    for rows in [vec![], vec![vec![Value::Int(1)]]] {
        let empty = rows.is_empty();
        let schema = RowSchema::new(vec!["x".into()]);
        let child = Box::new(crate::TableScan::from_physical_rows(
            schema,
            rows.into_iter()
                .map(crate::PhysicalRow::from_values)
                .collect(),
        ));
        let name = control.copy_text("v.missing").unwrap();
        let mut filter = Filter::with_unresolved_column(child, name)
            .into_boxed(&control)
            .unwrap();
        filter.open().unwrap();
        let result = filter.next();
        if empty {
            assert!(result.unwrap().is_none());
        } else {
            let error = crate::physical::physical_exec_error(result.unwrap_err());
            assert_eq!(error.sqlstate(), Some("XX000"));
            assert!(error.to_string().contains("v.missing"));
        }
        filter.close().unwrap();
        drop(filter);
        assert_eq!(memory.used(), 0);
    }
}
