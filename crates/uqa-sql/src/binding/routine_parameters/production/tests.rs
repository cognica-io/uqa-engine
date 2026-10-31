//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn cloned_parameter_namespaces_share_their_last_surviving_allowance() {
    let name = "argument".repeat(2048);
    let ty = ColumnType::Named("payload".repeat(2048));
    let columns = [(name.as_str(), Some(&ty))];
    let budget = MemoryBudget::new(4 * 1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let scope =
        RoutineParameterScope::new_with_control("function", columns.iter().copied(), &control)
            .unwrap();
    assert_eq!(scope.schema().qualified_slot("function", &name), Some(0));
    assert_eq!(
        scope.schema().internal_slot(ROUTINE_PARAMETERS.column(0)),
        Some(0)
    );
    let retained = budget.used();
    let held = budget.reserve(budget.limit() - retained).unwrap();
    let mut clone = None;
    let allocation = allocation_counter::measure(|| {
        clone = Some(scope.clone());
    });
    assert_eq!(allocation.count_total, 0);
    drop(scope);
    assert_eq!(budget.used(), retained + held.bytes());
    assert_eq!(
        clone
            .as_ref()
            .unwrap()
            .schema()
            .qualified_slot("function", &name),
        Some(0)
    );
    drop(clone);
    assert_eq!(budget.used(), held.bytes());
    drop(held);
    assert_eq!(budget.used(), 0);
}

#[test]
fn parameter_scope_admission_and_cancellation_precede_large_name_copies() {
    let name = "argument".repeat(32768);
    let columns = [(name.as_str(), Some(&ColumnType::Integer))];
    let budget = MemoryBudget::new(1024);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let allocation = allocation_counter::measure(|| {
        let error =
            RoutineParameterScope::new_with_control("function", columns.iter().copied(), &control)
                .err()
                .expect("scope quota");
        assert_eq!(error.sqlstate(), Some("53200"));
    });
    assert!(allocation.bytes_total < 4096, "{allocation:?}");
    assert_eq!(budget.used(), 0);
    for original_cancelled in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert!(control.check().is_err());
        let allocation = allocation_counter::measure(|| {
            let error = RoutineParameterScope::new_with_control(
                "function",
                columns.iter().copied(),
                &control,
            )
            .err()
            .expect("scope cancellation");
            assert_eq!(error.sqlstate(), Some("57014"));
        });
        assert_eq!(allocation.count_total, 0);
        assert_eq!(budget.used(), 0);
    }
}
