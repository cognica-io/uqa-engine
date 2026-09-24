//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn evaluate(name: &str, args: &[Value], control: &ProductionControl<'_>) -> Produced<Value> {
    eval_postgres_immutable_with_control(name, args, control)
        .unwrap()
        .unwrap()
}

#[test]
fn immutable_leafs_share_ordinary_null_arity_and_value_semantics() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let cases = [
        ("factorial", vec![Value::Int(0)], Value::Int(1)),
        (
            "factorial",
            vec![Value::Int(20)],
            Value::Int(2_432_902_008_176_640_000),
        ),
        ("factorial", vec![Value::Null], Value::Null),
        ("bit_length", vec![Value::Str("é界".into())], Value::Int(40)),
        (
            "bit_length",
            vec![Value::FixedChar("é  ".into())],
            Value::Int(16),
        ),
        (
            "bit_length",
            vec![Value::Bytes(vec![0, 255])],
            Value::Int(16),
        ),
        (
            "quote_ident",
            vec![Value::Str("a_1$".into())],
            Value::Str("a_1$".into()),
        ),
        (
            "quote_ident",
            vec![Value::Str("select".into())],
            Value::Str("\"select\"".into()),
        ),
        (
            "quote_ident",
            vec![Value::Str("a\"界".into())],
            Value::Str("\"a\"\"界\"".into()),
        ),
        (
            "quote_literal",
            vec![Value::Str("a'b\\c".into())],
            Value::Str("E'a''b\\\\c'".into()),
        ),
        (
            "quote_literal",
            vec![Value::Str("é'".into())],
            Value::Str("'é'''".into()),
        ),
        ("quote_literal", vec![Value::Null], Value::Null),
        (
            "quote_nullable",
            vec![Value::Null],
            Value::Str("NULL".into()),
        ),
        ("quote_nullable", vec![], Value::Str("NULL".into())),
        (
            "quote_nullable",
            vec![Value::Int(42)],
            Value::Str("'42'".into()),
        ),
        (
            "num_nulls",
            vec![Value::Null, Value::Int(1), Value::Null],
            Value::Int(2),
        ),
        (
            "num_nonnulls",
            vec![Value::Null, Value::Int(1), Value::Null],
            Value::Int(1),
        ),
    ];
    for (name, args, expected) in cases {
        let output = evaluate(name, &args, &control);
        assert_eq!(*output, expected, "{name}");
        assert_eq!(
            super::super::eval_postgres_functions(name, &args)
                .unwrap()
                .unwrap(),
            expected
        );
        assert_eq!(budget.used(), output.reserved_bytes());
        drop(output);
        assert_eq!(budget.used(), 0);
    }
    for (name, args) in [
        ("factorial", vec![]),
        ("bit_length", vec![]),
        ("quote_ident", vec![]),
        ("string_to_array", vec![Value::Null]),
    ] {
        let controlled = eval_postgres_immutable_with_control(name, &args, &control)
            .unwrap()
            .unwrap_err();
        let ordinary = super::super::eval_postgres_functions(name, &args)
            .unwrap()
            .unwrap_err();
        assert_eq!(controlled.to_string(), ordinary.to_string());
        assert_eq!(budget.used(), 0);
    }
    assert!(eval_postgres_immutable_with_control("string_to_table", &[], &control).is_none());
    assert!(eval_postgres_immutable_with_control("random", &[], &control).is_none());
}

#[test]
fn factorial_preserves_integer_decimal_and_existing_overflow_results() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for (argument, expected) in [
        (21, "51090942171709440000"),
        (33, "8683317618811886495518194401280000000"),
    ] {
        let output = evaluate("factorial", &[Value::Int(argument)], &control);
        let Value::Decimal(value) = &*output else {
            panic!("large factorial must remain exact numeric");
        };
        assert_eq!(value.to_sql_string(), expected);
        assert_eq!(budget.used(), output.reserved_bytes());
        assert_eq!(
            super::super::eval_postgres_functions("factorial", &[Value::Int(argument)])
                .unwrap()
                .unwrap(),
            *output
        );
        drop(output);
        assert_eq!(budget.used(), 0);
    }
    for (value, state) in [(-1, "2201F"), (34, "22003"), (i64::MAX, "22003")] {
        let error =
            eval_postgres_immutable_with_control("factorial", &[Value::Int(value)], &control)
                .unwrap()
                .unwrap_err();
        assert_eq!(error.sqlstate(), Some(state));
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn integer_base_dispatch_preserves_width_and_lowercase_output() {
    let budget = MemoryBudget::new(1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for (dispatch, input, expected) in [
        (
            FunctionDispatch::ToBinInt4,
            -1,
            "11111111111111111111111111111111",
        ),
        (
            FunctionDispatch::ToBinInt8,
            -1,
            "1111111111111111111111111111111111111111111111111111111111111111",
        ),
        (FunctionDispatch::ToHexInt4, -1, "ffffffff"),
        (FunctionDispatch::ToHexInt8, -1, "ffffffffffffffff"),
        (FunctionDispatch::ToOctInt4, -1, "37777777777"),
        (FunctionDispatch::ToOctInt8, -1, "1777777777777777777777"),
        (FunctionDispatch::ToHexInt8, i64::MIN, "8000000000000000"),
        (FunctionDispatch::ToBinInt4, 0, "0"),
    ] {
        let output =
            eval_postgres_integer_base_with_control(dispatch, &[Value::Int(input)], &control)
                .unwrap()
                .unwrap();
        assert_eq!(*output, Value::Str(expected.into()));
        assert_eq!(
            super::super::eval_dispatched_postgres_function(dispatch, &[Value::Int(input)])
                .unwrap()
                .unwrap(),
            *output
        );
        assert_eq!(budget.used(), output.reserved_bytes());
        drop(output);
        assert_eq!(budget.used(), 0);
    }
    for dispatch in [
        FunctionDispatch::ToBinInt4,
        FunctionDispatch::ToHexInt4,
        FunctionDispatch::ToOctInt4,
    ] {
        let error =
            eval_postgres_integer_base_with_control(dispatch, &[Value::Int(i64::MAX)], &control)
                .unwrap()
                .unwrap_err();
        assert_eq!(error.sqlstate(), Some("22003"));
        let output = eval_postgres_integer_base_with_control(dispatch, &[Value::Null], &control)
            .unwrap()
            .unwrap();
        assert_eq!(*output, Value::Null);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn string_array_preserves_empty_delimiters_unicode_markers_and_one_based_bounds() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let cases = [
        ("", Value::Null, None, vec![]),
        ("", Value::Str(",".into()), None, vec![]),
        (
            "a,b",
            Value::Str("".into()),
            None,
            vec![Value::Str("a,b".into())],
        ),
        (
            "a,,b,",
            Value::Str(",".into()),
            Some(Value::Str("".into())),
            vec![
                Value::Str("a".into()),
                Value::Null,
                Value::Str("b".into()),
                Value::Null,
            ],
        ),
        (
            "é界x",
            Value::Null,
            Some(Value::Str("界".into())),
            vec![Value::Str("é".into()), Value::Null, Value::Str("x".into())],
        ),
        (
            "x::NULL::y",
            Value::Str("::".into()),
            Some(Value::Str("NULL".into())),
            vec![Value::Str("x".into()), Value::Null, Value::Str("y".into())],
        ),
    ];
    for (text, separator, marker, expected) in cases {
        let mut args = vec![Value::Str(text.into()), separator];
        if let Some(marker) = marker {
            args.push(marker);
        }
        let output = evaluate("string_to_array", &args, &control);
        let Value::Array(array) = &*output else {
            panic!("expected array");
        };
        assert_eq!(array.elements(), expected);
        assert_eq!(
            array.lower_bounds(),
            if expected.is_empty() {
                &[][..]
            } else {
                &[1][..]
            }
        );
        assert_eq!(
            super::super::eval_postgres_functions("string_to_array", &args)
                .unwrap()
                .unwrap(),
            *output
        );
        assert_eq!(budget.used(), output.reserved_bytes());
        drop(output);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn scalar_outputs_admit_allocations_and_release_partial_construction() {
    let cases = [
        ("factorial", vec![Value::Int(21)]),
        ("quote_ident", vec![Value::Str("a\"b".into())]),
        ("quote_literal", vec![Value::Str("a'b\\c".into())]),
        ("quote_nullable", vec![Value::Null]),
        (
            "string_to_array",
            vec![Value::Str("a,b,c,d,e,f,g,h".into()), Value::Str(",".into())],
        ),
    ];
    let token = CancellationToken::new();
    let budget = MemoryBudget::new(0);
    let control = ProductionControl::new(&budget, &token, &token);
    for dispatch in [
        FunctionDispatch::ToBinInt4,
        FunctionDispatch::ToHexInt8,
        FunctionDispatch::ToOctInt8,
    ] {
        let error = eval_postgres_integer_base_with_control(dispatch, &[Value::Int(-1)], &control)
            .unwrap()
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
        assert_eq!(budget.used(), 0);
    }
    for (name, args) in cases {
        let budget = MemoryBudget::new(0);
        let control = ProductionControl::new(&budget, &token, &token);
        let error = eval_postgres_immutable_with_control(name, &args, &control)
            .unwrap()
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"), "{name}");
        assert_eq!(budget.used(), 0);
    }
    // These limits enter progressively later native-buffer replacements; every failed construction must release all earlier text, values, dimensions, and header leases.
    let args = [Value::Str("a,b,c,d,e,f,g,h".into()), Value::Str(",".into())];
    let mut observed_failure = false;
    let mut observed_success = false;
    for limit in [32, 64, 128, 256, 512, 1024, 4096] {
        let budget = MemoryBudget::new(limit);
        let control = ProductionControl::new(&budget, &token, &token);
        match eval_postgres_immutable_with_control("string_to_array", &args, &control).unwrap() {
            Ok(output) => {
                observed_success = true;
                assert_eq!(budget.used(), output.reserved_bytes());
            }
            Err(error) => {
                observed_failure = true;
                assert_eq!(error.sqlstate(), Some("53200"));
            }
        }
        assert_eq!(budget.used(), 0);
    }
    assert!(observed_failure && observed_success);
}

#[test]
fn scalar_and_integer_dispatch_check_both_cancellation_owners() {
    for original_cancelled in [false, true] {
        let budget = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        for name in [
            "factorial",
            "bit_length",
            "to_hex",
            "string_to_array",
            "quote_ident",
            "quote_literal",
            "quote_nullable",
            "num_nulls",
            "num_nonnulls",
        ] {
            let error = eval_postgres_immutable_with_control(name, &[], &control)
                .unwrap()
                .unwrap_err();
            assert_eq!(error.sqlstate(), Some("57014"), "{name}");
            assert_eq!(budget.used(), 0);
        }
        let error =
            eval_postgres_integer_base_with_control(FunctionDispatch::ToHexInt4, &[], &control)
                .unwrap()
                .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), 0);
    }
    let budget = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    assert_eq!(
        *evaluate("bit_length", &[Value::Str("borrowed".into())], &control),
        Value::Int(64)
    );
    assert_eq!(
        *evaluate("num_nulls", &[Value::Null], &control),
        Value::Int(1)
    );
    assert_eq!(budget.used(), 0);
}
