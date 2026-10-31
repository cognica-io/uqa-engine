//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::CommandPlan;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};

fn large_command() -> UnifiedPlan {
    UnifiedPlan::Command(Box::new(CommandPlan::SetVariable {
        name: "application_name".into(),
        value: "x".repeat(4 * 1024 * 1024),
        local: false,
        is_default: false,
    }))
}

#[test]
fn analyzed_cache_handles_share_complete_utility_syntax_without_copying_payloads() {
    let scopes = Scopes::default();
    let context = StatementAnalysisContext {
        scopes: &scopes,
        routines: &NoRoutines,
        aliases: &NoRoutines,
    };
    let (plan, cached) = analyze_for_statement_reuse(&context, large_command(), &[]).unwrap();
    let cached = cached.unwrap();
    drop(plan);
    let mut shared = None;
    let allocation = allocation_counter::measure(|| {
        shared = Some(cached.clone());
    });
    assert_eq!(allocation.count_total, 0, "{allocation:?}");
    let shared = shared.unwrap();
    assert!(cached.ptr_eq(&shared));
    assert!(std::ptr::eq(
        cached.plan_for(&[]).unwrap(),
        shared.plan_for(&[]).unwrap()
    ));
    drop(cached);
    let UnifiedPlan::Command(command) = shared.plan_for(&[]).unwrap() else {
        panic!("utility syntax");
    };
    let CommandPlan::SetVariable { value, .. } = command.as_ref() else {
        panic!("setting");
    };
    assert_eq!(value.len(), 4 * 1024 * 1024);
}

#[test]
fn analyzed_cache_capture_retains_exact_storage_until_the_last_handle_drops() {
    let source = large_command();
    let params = [SQLParam::TypedScalar {
        value: Value::Null,
        ty: ColumnType::Array(Box::new(ColumnType::Named("t".repeat(4096)))),
    }];
    let budget = MemoryBudget::new(8 * 1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut cached = None;
    let allocation = allocation_counter::measure(|| {
        cached = AnalyzedStatement::capture_with_control(&source, &params, &control).unwrap();
    });
    let cached = cached.unwrap();
    let required = cached.reserved_bytes();
    assert!(required > 4 * 1024 * 1024 + 4096);
    assert_eq!(budget.used(), required);
    assert_eq!(
        allocation.bytes_current,
        (required + 2 * size_of::<usize>()) as i64,
        "only Arc reference counters are outside the payload allowance: {allocation:?}"
    );
    let mut shared = None;
    let allocation = allocation_counter::measure(|| {
        shared = Some(cached.clone());
    });
    assert_eq!(allocation.count_total, 0, "{allocation:?}");
    drop(cached);
    assert_eq!(budget.used(), required);
    let shared = shared.unwrap();
    assert!(shared.plan_for(&params).is_some());
    drop(shared);
    assert_eq!(budget.used(), 0);
    let exact = budget.child(required);
    let captured = AnalyzedStatement::capture_with_control(
        &source,
        &params,
        &ProductionControl::new(&exact, &token, &token),
    )
    .unwrap()
    .unwrap();
    assert_eq!(captured.reserved_bytes(), required);
    drop(captured);
    assert_eq!(budget.used(), 0);
    let insufficient = budget.child(required - 1);
    let error = AnalyzedStatement::capture_with_control(
        &source,
        &params,
        &ProductionControl::new(&insufficient, &token, &token),
    )
    .err()
    .unwrap();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(budget.used(), 0);
}

#[test]
fn analyzed_cache_capture_admits_parameter_types_without_retaining_argument_payloads() {
    let source = UnifiedPlan::Command(Box::new(CommandPlan::Empty));
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let params = [SQLParam::Scalar(Value::Str("x".repeat(4 * 1024 * 1024)))];
    let captured = AnalyzedStatement::capture_with_control(&source, &params, &control)
        .unwrap()
        .unwrap();
    assert!(captured.reserved_bytes() < 4096);
    assert!(captured
        .plan_for(&[SQLParam::Scalar(Value::Str("different".into()))])
        .is_some());
    drop(captured);
    let params = [SQLParam::TypedScalar {
        value: Value::Null,
        ty: ColumnType::Named("t".repeat(4 * 1024 * 1024)),
    }];
    let allocation = allocation_counter::measure(|| {
        let error = AnalyzedStatement::capture_with_control(&source, &params, &control)
            .err()
            .unwrap();
        assert_eq!(error.sqlstate(), Some("53200"));
    });
    assert!(allocation.bytes_total < 16 * 1024, "{allocation:?}");
    assert_eq!(allocation.bytes_current, 0, "{allocation:?}");
    assert_eq!(budget.used(), 0);
}

#[test]
fn analyzed_cache_capture_preserves_ancestor_allowance_cancellation_and_unwind_cleanup() {
    let source = large_command();
    let budget = MemoryBudget::new(8 * 1024 * 1024);
    let token = CancellationToken::new();
    let sibling = budget.reserve(budget.available() - 4096).unwrap();
    let child = budget.child(8 * 1024 * 1024);
    let allocation = allocation_counter::measure(|| {
        let error = AnalyzedStatement::capture_with_control(
            &source,
            &[],
            &ProductionControl::new(&child, &token, &token),
        )
        .err()
        .unwrap();
        assert_eq!(error.sqlstate(), Some("53200"));
    });
    assert!(allocation.bytes_total < 16 * 1024, "{allocation:?}");
    assert_eq!(budget.used(), sibling.bytes());
    drop(sibling);
    for cancel_original in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let error = AnalyzedStatement::capture_with_control(
            &source,
            &[],
            &ProductionControl::new(&child, &original, &invoking),
        )
        .err()
        .unwrap();
        assert!(matches!(error.primary(), SQLError::Cancelled(_)));
        assert_eq!(budget.used(), 0);
    }
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _captured = AnalyzedStatement::capture_with_control(
            &source,
            &[],
            &ProductionControl::new(&child, &token, &token),
        )
        .unwrap();
        panic!("cache consumer unwind");
    }))
    .is_err());
    assert_eq!(budget.used(), 0);
}
