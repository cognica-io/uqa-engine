//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{FunctionDispatch, NumericOperator};
use uqa_core::{memory::MemoryBudget, ArrayValue, CancellationToken};

fn args(
    values: &[Value],
    control: &ProductionControl<'_>,
) -> Produced<Vec<(Option<String>, Value)>> {
    let mut output = ProductionVec::new(*control);
    for value in values {
        let (value, memory) = control.copy_value(value).unwrap().into_parts();
        output
            .push_produced(control.finish((None, value), memory).unwrap())
            .unwrap();
    }
    output.finish().unwrap()
}

#[test]
fn moving_generated_arguments_keeps_large_payload_identity_and_its_allowance() {
    let budget = MemoryBudget::new(48 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let input = args(&[Value::Str("x".repeat(32 * 1024))], &control);
    let Value::Str(text) = &input[0].1 else {
        panic!("text argument");
    };
    let pointer = text.as_ptr();
    let moved = MovedArguments::new(input, &control).unwrap();
    let Value::Str(text) = &moved.values[0] else {
        panic!("text argument");
    };
    assert_eq!(text.as_ptr(), pointer);
    assert_eq!(text.len(), 32 * 1024);
    assert!(budget.used() >= text.capacity());
    drop(moved);
    assert_eq!(budget.used(), 0);
}

#[test]
fn generated_dispatch_retains_results_from_shared_scalar_families() {
    let budget = MemoryBudget::new(128 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for (name, values, expected) in [
        (
            "UPPER",
            vec![Value::Str("hello".into())],
            Value::Str("HELLO".into()),
        ),
        ("sin", vec![Value::Int(0)], Value::Float(0.0)),
        ("to_hex", vec![Value::Int(255)], Value::Str("ff".into())),
        (
            "regexp_substr",
            vec![Value::Str("é12".into()), Value::Str("[0-9]+".into())],
            Value::Str("12".into()),
        ),
        (
            "jsonb_array_length",
            vec![Value::JsonB("[1,null,3]".into())],
            Value::Int(3),
        ),
        (
            "array_append",
            vec![Value::Null, Value::Int(7)],
            Value::Array(ArrayValue::try_new(vec![Value::Int(7)]).unwrap()),
        ),
    ] {
        let result = eval_generated_function_call_with_control(
            name,
            None,
            args(&values, &control),
            &control,
        )
        .unwrap();
        assert_eq!(*result, expected, "{name}");
        assert_eq!(budget.used(), result.reserved_bytes());
        drop(result);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn bound_generated_dispatch_keeps_structural_identity_and_fixed_integer_overflow() {
    let budget = MemoryBudget::new(64 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let binding = FunctionBinding::dispatched(FunctionDispatch::IsDistinct);
    let result = eval_generated_function_call_with_control(
        "unused_display_name",
        Some(&binding),
        args(&[Value::Null, Value::Null], &control),
        &control,
    )
    .unwrap();
    assert_eq!(*result, Value::Bool(false));
    drop(result);
    let mut binding =
        FunctionBinding::dispatched(FunctionDispatch::NumericOperator(NumericOperator::Absolute));
    binding.argument_types = vec!["smallint".into()];
    assert_eq!(
        eval_generated_function_call_with_control(
            "unused_display_name",
            Some(&binding),
            args(&[Value::Int(-32768)], &control),
            &control
        )
        .unwrap_err()
        .sqlstate(),
        Some("22003")
    );
    let fixed = FunctionBinding {
        object_id: None,
        name: "pg_catalog.abs".into(),
        argument_types: vec!["integer".into()],
        builtin: true,
        dispatch: None,
        invocation: None,
        resolution_error: None,
    };
    assert_eq!(
        eval_generated_function_call_with_control(
            "abs",
            Some(&fixed),
            args(&[Value::Int(i64::from(i32::MIN))], &control),
            &control
        )
        .unwrap_err()
        .sqlstate(),
        Some("22003")
    );
    assert_eq!(budget.used(), 0);
}

#[test]
fn generated_dispatch_quota_and_cancellation_preserve_earlier_results() {
    let budget = MemoryBudget::new(8192);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let held = control.copy_text("earlier").unwrap();
    let retained = budget.used();
    let arguments = args(&[Value::Str("x".repeat(32)), Value::Int(1024)], &control);
    assert_eq!(
        eval_generated_function_call_with_control("repeat", None, arguments, &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(budget.used(), retained);
    for cancel_original in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let arguments = args(&[Value::Str("later".into())], &control);
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert_eq!(
            eval_generated_function_call_with_control("upper", None, arguments, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), retained);
    }
    assert_eq!(&*held, "earlier");
    drop(held);
    assert_eq!(budget.used(), 0);
}
