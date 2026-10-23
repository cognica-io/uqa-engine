//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::scalar::evaluator::eval_scalar_inner;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};

#[test]
fn prepared_row_constructors_share_descriptors_without_reusing_dynamic_types() {
    let prepared = PreparedExpressions::scalar(ScalarExpr::Row(vec![ScalarExpr::Param(1)]));
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let make = |value: Value| {
        let params = [uqa_sql::SQLParam::Scalar(value)];
        let context = ScalarEvalContext::new(None, &params).with_function_states(prepared.calls());
        eval_scalar_inner(&prepared, &context, &control).unwrap()
    };
    let first = make(Value::Int(1));
    let second = make(Value::Int(2));
    let Value::Row(first_row) = &*first else {
        panic!("row")
    };
    let Value::Row(second_row) = &*second else {
        panic!("row")
    };
    assert!(first_row
        .descriptor()
        .unwrap()
        .shares_storage_with(second_row.descriptor().unwrap()));
    assert_eq!(second_row.values(), &[Value::Int(2)]);
    let changed = make(Value::Str("text".into()));
    let Value::Row(changed_row) = &*changed else {
        panic!("row")
    };
    assert!(!first_row
        .descriptor()
        .unwrap()
        .shares_storage_with(changed_row.descriptor().unwrap()));
    assert_ne!(first_row.field_types(), changed_row.field_types());
    let restored = make(Value::Int(3));
    let Value::Row(restored_row) = &*restored else {
        panic!("row")
    };
    assert!(first_row
        .descriptor()
        .unwrap()
        .shares_storage_with(restored_row.descriptor().unwrap()));
    drop((first, changed, restored, prepared));
    assert_eq!(budget.used(), second.reserved_bytes());
    drop(second);
    assert_eq!(budget.used(), 0);
}

#[test]
fn prepared_row_descriptor_state_is_independent_after_clone_and_obeys_cancellation() {
    let prepared =
        PreparedExpressions::scalar(ScalarExpr::Row(vec![ScalarExpr::Literal(Value::Int(1))]));
    let control = ProductionControl::uncontrolled();
    let evaluate = |prepared: &PreparedExpressions<ScalarExpr>| {
        eval_scalar_inner(
            prepared,
            &ScalarEvalContext::new(None, &[]).with_function_states(prepared.calls()),
            &control,
        )
        .unwrap()
    };
    let first = evaluate(&prepared);
    let cloned = prepared.clone();
    let second = evaluate(&cloned);
    let (Value::Row(first), Value::Row(second)) = (&*first, &*second) else {
        panic!("rows")
    };
    assert_eq!(first.field_types(), second.field_types());
    assert!(!first
        .descriptor()
        .unwrap()
        .shares_storage_with(second.descriptor().unwrap()));
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    token.cancel();
    let cancelled = ProductionControl::new(&budget, &token, &token);
    assert_eq!(
        eval_scalar_inner(
            &prepared,
            &ScalarEvalContext::new(None, &[]).with_function_states(prepared.calls()),
            &cancelled
        )
        .unwrap_err()
        .sqlstate(),
        Some("57014")
    );
    assert_eq!(budget.used(), 0);
}
