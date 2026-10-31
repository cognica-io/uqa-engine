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
use uqa_core::{memory::MemoryBudget, CancellationToken};

struct Input {
    schema: RowSchema,
    closes: Arc<AtomicUsize>,
}
impl PhysicalOperator for Input {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }
    fn open(&mut self) -> ExecResult<()> {
        panic!("construction must not open")
    }
    fn next(&mut self) -> ExecResult<Option<Batch>> {
        panic!("construction must not read")
    }
    fn close(&mut self) -> ExecResult<()> {
        self.closes.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}
struct Source(Arc<AtomicUsize>);
impl LateralSource<'static> for Source {
    fn rows_for(&mut self, _: &OwnedPhysicalRow) -> ExecResult<LateralRows<'static>> {
        panic!("construction must not evaluate")
    }
    fn admits(&mut self, _: &OwnedPhysicalRow) -> ExecResult<bool> {
        panic!("construction must not evaluate")
    }
    fn matches(&mut self, _: &OwnedPhysicalRow) -> ExecResult<bool> {
        panic!("construction must not evaluate")
    }
    fn close(&mut self) -> ExecResult<()> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

#[test]
fn lateral_construction_retires_both_inputs_on_admission_cancellation_and_wrapper_failure() {
    let memory = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    let left_closes = Arc::new(AtomicUsize::new(0));
    let right_closes = Arc::new(AtomicUsize::new(0));
    let build = || {
        LateralJoin::with_right_schema_with_control(
            Box::new(Input {
                schema: RowSchema::with_qualified_types("l", vec!["id".into()], vec![None]),
                closes: left_closes.clone(),
            }),
            Box::new(Source(right_closes.clone())),
            JoinKind::Left,
            ResultRow::new(),
            ResultRow::new(),
            RowSchema::with_qualified_types("r", vec!["id".into()], vec![None]),
            &control,
        )
    };
    let occupied = memory.reserve(memory.available()).unwrap();
    assert_eq!(build().err().unwrap().sqlstate(), Some("53200"));
    assert_eq!(memory.used(), occupied.bytes());
    assert_eq!(left_closes.load(Ordering::Relaxed), 1);
    assert_eq!(right_closes.load(Ordering::Relaxed), 1);
    drop(occupied);
    for (index, token) in [&original, &invoking].into_iter().enumerate() {
        token.cancel();
        assert_eq!(build().err().unwrap().sqlstate(), Some("57014"));
        assert_eq!(memory.used(), 0);
        assert_eq!(left_closes.load(Ordering::Relaxed), index + 2);
        assert_eq!(right_closes.load(Ordering::Relaxed), index + 2);
        token.reset();
    }
    let join = build().unwrap();
    let occupied = memory.reserve(memory.available()).unwrap();
    assert_eq!(
        join.into_boxed(&control).err().unwrap().sqlstate(),
        Some("53200")
    );
    assert_eq!(memory.used(), occupied.bytes());
    assert_eq!(left_closes.load(Ordering::Relaxed), 4);
    assert_eq!(right_closes.load(Ordering::Relaxed), 4);
    drop(occupied);
    assert_eq!(memory.used(), 0);
}
