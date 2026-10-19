//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{
    ast::{FunctionBinding, FunctionDispatch},
    expr::EvalContext,
};
use uqa_core::{
    memory::{MemoryBudget, ProductionControl, ProductionVec},
    CancellationToken, Value,
};

#[test]
fn type_observation_retains_declared_identity_and_releases_consumed_inputs() {
    let budget = MemoryBudget::new(40 * 1024);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let held = control.copy_text("earlier result").unwrap();
    let retained = budget.used();
    let context = EvalContext::new(None, &[]);
    for (type_oid, value) in [
        (21, Value::Null),
        (21, Value::Int(7)),
        (25, Value::Str("x".repeat(32 * 1024))),
    ] {
        let binding = FunctionBinding::dispatched(FunctionDispatch::TypeOf { type_oid });
        assert_eq!(
            crate::expr::bound_scalar_function_strictness(&binding.name, Some(&binding), 1),
            Some(false)
        );
        assert_eq!(
            crate::expr::eval_bound_builtin_function_call(
                &binding,
                vec![(None, value.clone())],
                &context
            )
            .unwrap(),
            Value::Int(i64::from(type_oid))
        );
        let (value, memory) = control.copy_value(&value).unwrap().into_parts();
        let mut arguments = ProductionVec::new(control);
        arguments
            .push_produced(control.finish((None, value), memory).unwrap())
            .unwrap();
        let output = crate::expr::eval_builtin_function_call_with_control(
            &binding.name,
            Some(&binding),
            arguments.finish().unwrap(),
            &control,
            &context,
        )
        .unwrap();
        assert_eq!(*output, Value::Int(i64::from(type_oid)));
        assert_eq!(budget.used(), retained + output.reserved_bytes());
        drop(output);
        assert_eq!(budget.used(), retained);
    }
    let binding = FunctionBinding::dispatched(FunctionDispatch::TypeOf { type_oid: 25 });
    let (value, memory) = control
        .copy_value(&Value::Str("x".repeat(4096)))
        .unwrap()
        .into_parts();
    let mut arguments = ProductionVec::new(control);
    arguments
        .push_produced(control.finish((None, value), memory).unwrap())
        .unwrap();
    let arguments = arguments.finish().unwrap();
    invoking.cancel();
    let error = crate::expr::eval_builtin_function_call_with_control(
        &binding.name,
        Some(&binding),
        arguments,
        &control,
        &context,
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("57014"));
    assert_eq!(budget.used(), retained);
    drop(held);
    assert_eq!(budget.used(), 0);
}
