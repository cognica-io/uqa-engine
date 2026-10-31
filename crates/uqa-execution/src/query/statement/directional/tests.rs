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

struct Task(Arc<AtomicUsize>);
impl Drop for Task {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
impl DirectionalQueryTask for Task {
    fn execute(self: Box<Self>, _: Rc<dyn QueryRowConsumer>) -> Result<(), SQLError> {
        Err(SQLError::Routine {
            sqlstate: "22012".into(),
            message: "directional child failure".into(),
        })
    }
}

#[test]
fn directional_wrapper_admission_retires_rejected_closed_and_failed_tasks() {
    let budget = MemoryBudget::new(64 << 10);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let dropped = Arc::new(AtomicUsize::new(0));
    let create = || {
        DirectionalQueryPlanOperator::new_with_control(
            Box::new(Task(dropped.clone())),
            BackwardScanSupport::Native,
            RowSchema::default(),
            &control,
        )
    };
    let occupied = budget.reserve(budget.available()).unwrap();
    let error = create().err().unwrap();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    assert_eq!(budget.used(), occupied.bytes());
    drop(occupied);
    for token in [&original, &invoking] {
        token.cancel();
        assert_eq!(create().err().unwrap().sqlstate(), Some("57014"));
        assert_eq!(budget.used(), 0);
        token.reset();
    }
    let mut operator = create().unwrap();
    assert_eq!(budget.used(), size_of::<DirectionalQueryPlanOperator>());
    operator.close().unwrap();
    assert_eq!(dropped.load(Ordering::SeqCst), 4);
    assert_eq!(budget.used(), size_of::<DirectionalQueryPlanOperator>());
    drop(operator);
    assert_eq!(budget.used(), 0);
    let mut operator = create().unwrap();
    operator.open().unwrap();
    let error = operator.next().unwrap_err();
    assert!(matches!(error, ExecError::SQL(ref error) if error.sqlstate() == Some("22012")));
    operator.close().unwrap();
    assert_eq!(dropped.load(Ordering::SeqCst), 5);
    drop(operator);
    assert_eq!(budget.used(), 0);
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _operator = create().unwrap();
        panic!("retire pending directional input");
    }));
    assert!(panic.is_err());
    assert_eq!(dropped.load(Ordering::SeqCst), 6);
    assert_eq!(budget.used(), 0);
}
