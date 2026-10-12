//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{FunctionBinding, FunctionDispatch};
use uqa_core::{
    memory::{MemoryBudget, ProductionVec},
    CancellationToken,
};

#[test]
fn collation_observation_keeps_input_ownership_and_enforces_output_limits() {
    let budget = MemoryBudget::new(40 * 1024);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let held = control.copy_text("earlier result").unwrap();
    let retained = budget.used();
    let binding = FunctionBinding::dispatched(FunctionDispatch::CollationFor {
        collation_oid: 950,
        noncollatable_type_oid: None,
    });
    let (value, memory) = control
        .copy_value(&Value::Str("x".repeat(32 * 1024)))
        .unwrap()
        .into_parts();
    let mut args = ProductionVec::new(control);
    args.push_produced(control.finish((None, value), memory).unwrap())
        .unwrap();
    let output = crate::expr::eval_builtin_function_call_with_control(
        "pg_collation_for",
        Some(&binding),
        args.finish().unwrap(),
        &control,
        &crate::expr::EvalContext::new(None, &[]),
    )
    .unwrap();
    assert_eq!(*output, Value::Str("\"C\"".into()));
    assert_eq!(budget.used(), retained + output.reserved_bytes());
    drop(output);
    let available = budget.limit() - budget.used();
    let guard = control.reserve(available).unwrap();
    assert_eq!(
        evaluate(950, None, None, &control).unwrap_err().sqlstate(),
        Some("53200")
    );
    drop(guard);
    invoking.cancel();
    assert_eq!(
        evaluate(0, None, None, &control).unwrap_err().sqlstate(),
        Some("57014")
    );
    assert_eq!(budget.used(), retained);
    drop(held);
    assert_eq!(budget.used(), 0);
}
