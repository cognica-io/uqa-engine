//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, ArrayValue, CancellationToken, DecimalValue};

#[test]
fn controlled_range_constructors_preserve_null_bounds_flags_variadic_arrays_and_normalization() {
    let memory = MemoryBudget::new(1024 * 1024);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    let cases = [
        (
            "int4range",
            vec![Value::Int(1), Value::Int(4), Value::Str("(]".into())],
            "[2,5)",
        ),
        (
            "int8range",
            vec![Value::Int(2_147_483_648), Value::Int(2_147_483_650)],
            "[2147483648,2147483650)",
        ),
        (
            "numrange",
            vec![
                Value::Decimal(DecimalValue::parse("1.00").unwrap()),
                Value::Decimal(DecimalValue::parse("4.000").unwrap()),
            ],
            "[1.00,4.000)",
        ),
        (
            "daterange",
            vec![
                Value::Str("2024-01-01".into()),
                Value::Str("2024-01-02".into()),
            ],
            "[2024-01-01,2024-01-02)",
        ),
        ("tsrange", vec![Value::Null, Value::Null], "(,)"),
        ("tstzrange", vec![Value::Null, Value::Null], "(,)"),
        (
            "int4multirange",
            vec![
                Value::Str("[10,12)".into()),
                Value::Str("[1,3)".into()),
                Value::Str("[3,5)".into()),
            ],
            "{[1,5),[10,12)}",
        ),
        (
            "nummultirange",
            vec![Value::Array(
                ArrayValue::try_new(vec![
                    Value::Str("[1.00,2.0)".into()),
                    Value::Str("[2,4.000)".into()),
                ])
                .unwrap(),
            )],
            "{[1.00,4.000)}",
        ),
    ];
    for (name, args, expected) in cases {
        let output = eval_range_functions_with_control(name, &args, &control)
            .unwrap()
            .unwrap();
        assert_eq!(&*output, &Value::Str(expected.into()));
        assert_eq!(memory.used(), output.reserved_bytes());
        assert_eq!(eval_range_functions(name, &args).unwrap().unwrap(), *output);
        drop(output);
        assert_eq!(memory.used(), 0);
    }
    let null = eval_range_functions_with_control("int4multirange", &[Value::Null], &control)
        .unwrap()
        .unwrap();
    assert_eq!(&*null, &Value::Null);
    assert_eq!(memory.used(), 0);
    assert!(eval_range_functions_with_control("unrelated", &[], &control).is_none());
    let invalid = eval_range_functions_with_control(
        "int4range",
        &[Value::Int(1), Value::Int(2), Value::Str("bad".into())],
        &control,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(invalid.sqlstate(), Some("22000"));
    assert_eq!(memory.used(), 0);
}

fn assert_bound_cases(
    cases: impl IntoIterator<Item = (RangeFunctionOperation, bool, Vec<&'static str>, Value)>,
) {
    let memory = MemoryBudget::new(1024 * 1024);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&memory, &cancellation, &cancellation);
    for (operation, multirange, args, expected) in cases {
        let args = args
            .into_iter()
            .map(|text| Value::Str(text.into()))
            .collect::<Vec<_>>();
        let output = eval_dispatched_range_function_with_control(
            operation,
            RangeSubtype::Integer,
            multirange,
            &args,
            &control,
        )
        .unwrap();
        assert_eq!(&*output, &expected);
        assert_eq!(memory.used(), output.reserved_bytes());
        assert_eq!(
            eval_dispatched_range_function(operation, RangeSubtype::Integer, multirange, &args)
                .unwrap(),
            *output
        );
        drop(output);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn controlled_bound_range_accessors_preserve_empty_and_infinite_endpoints() {
    use RangeFunctionOperation as Operation;
    assert_bound_cases([
        (Operation::Lower, false, vec!["[1,3)"], Value::Int(1)),
        (
            Operation::Upper,
            true,
            vec!["{[1,3),[10,12)}"],
            Value::Int(12),
        ),
        (Operation::IsEmpty, false, vec!["empty"], Value::Bool(true)),
        (Operation::IsEmpty, true, vec!["{}"], Value::Bool(true)),
        (
            Operation::LowerInclusive,
            false,
            vec!["[1,3)"],
            Value::Bool(true),
        ),
        (
            Operation::UpperInclusive,
            false,
            vec!["[1,3)"],
            Value::Bool(false),
        ),
        (
            Operation::LowerInfinite,
            false,
            vec!["(,3)"],
            Value::Bool(true),
        ),
        (
            Operation::UpperInfinite,
            false,
            vec!["[1,)"],
            Value::Bool(true),
        ),
    ]);
}

#[test]
fn controlled_bound_range_covers_and_relationships_preserve_disjoint_and_adjacent_members() {
    use RangeFunctionOperation as Operation;
    assert_bound_cases([
        (
            Operation::Merge,
            false,
            vec!["[1,3)", "[10,12)"],
            Value::Str("[1,12)".into()),
        ),
        (
            Operation::Merge,
            true,
            vec!["{[1,3),[10,12)}"],
            Value::Str("[1,12)".into()),
        ),
        (
            Operation::Multirange,
            false,
            vec!["[1,3)", "[3,5)"],
            Value::Str("{[1,5)}".into()),
        ),
        (
            Operation::Overlap,
            true,
            vec!["{[1,3),[10,12)}", "[11,15)"],
            Value::Bool(true),
        ),
        (
            Operation::Contains,
            true,
            vec!["{[1,3),[10,12)}", "[10,11)"],
            Value::Bool(true),
        ),
        (
            Operation::ContainedBy,
            false,
            vec!["[10,11)", "{[1,3),[10,12)}"],
            Value::Bool(true),
        ),
        (
            Operation::Adjacent,
            false,
            vec!["[1,3)", "[3,5)"],
            Value::Bool(true),
        ),
        (
            Operation::Adjacent,
            true,
            vec!["{[1,3),[5,7)}", "[3,5)"],
            Value::Bool(true),
        ),
    ]);
}

#[test]
fn numeric_range_accessors_and_covers_retain_only_the_produced_endpoint_or_text() {
    let memory = MemoryBudget::new(1024 * 1024);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&memory, &cancellation, &cancellation);
    let output = eval_dispatched_range_function_with_control(
        RangeFunctionOperation::Lower,
        RangeSubtype::Numeric,
        false,
        &[Value::Str("[-123456789012345678901234567890.000,2)".into())],
        &control,
    )
    .unwrap();
    assert_eq!(
        &*output,
        &Value::Decimal(DecimalValue::parse("-123456789012345678901234567890.000").unwrap())
    );
    assert!(output.reserved_bytes() > 0);
    assert_eq!(memory.used(), output.reserved_bytes());
    drop(output);
    let output = eval_dispatched_range_function_with_control(
        RangeFunctionOperation::Merge,
        RangeSubtype::Numeric,
        true,
        &[Value::Str("{[1.000,2.0),[10,12.00)}".into())],
        &control,
    )
    .unwrap();
    assert_eq!(&*output, &Value::Str("[1.000,12.00)".into()));
    drop(output);
    assert_eq!(memory.used(), 0);
}

#[test]
fn controlled_range_builtins_release_partial_inputs_and_preserve_prior_owners() {
    for allowance in [8, 32, 128, 512, 2048, 16384] {
        let memory = MemoryBudget::new(allowance);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&memory, &original, &invoking);
        let prior = control.copy_text("prior").unwrap();
        let before = memory.used();
        let args = [
            Value::Str("[1.000,2.0)".into()),
            Value::Str("[2,4.00)".into()),
        ];
        match eval_range_functions_with_control("nummultirange", &args, &control).unwrap() {
            Ok(value) => drop(value),
            Err(error) => assert_eq!(error.sqlstate(), Some("53200")),
        }
        assert_eq!(memory.used(), before);
        for token in [&original, &invoking] {
            token.cancel();
            assert_eq!(
                eval_range_functions_with_control("nummultirange", &args, &control)
                    .unwrap()
                    .unwrap_err()
                    .sqlstate(),
                Some("57014")
            );
            assert_eq!(
                eval_dispatched_range_function_with_control(
                    RangeFunctionOperation::Lower,
                    RangeSubtype::Numeric,
                    false,
                    &args[..1],
                    &control
                )
                .unwrap_err()
                .sqlstate(),
                Some("57014")
            );
            token.reset();
            assert_eq!(memory.used(), before);
        }
        assert_eq!(&**prior, "prior");
        drop(prior);
        assert_eq!(memory.used(), 0);
    }
    let memory = MemoryBudget::new(16384);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&memory, &cancellation, &cancellation);
    let result = eval_range_functions_with_control(
        "nummultirange",
        &[
            Value::Str("[12345678901234567890.00,12345678901234567891.0)".into()),
            Value::Str("bad".into()),
        ],
        &control,
    )
    .unwrap();
    assert_eq!(result.unwrap_err().sqlstate(), Some("22P02"));
    assert_eq!(memory.used(), 0);
}
