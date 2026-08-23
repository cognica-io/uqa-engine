//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::casting::cast_integer;
use super::*;
use crate::ast::Expr;

#[test]
fn literal_passthrough() {
    let ctx = EvalContext::new(None, &[]);
    let got = eval(&Expr::Literal(Value::Int(42)), &ctx).unwrap();
    assert_eq!(got, Value::Int(42));
}

#[test]
fn param_scalar_returns_value() {
    let params = vec![SQLParam::Scalar(Value::Str("hi".into()))];
    let ctx = EvalContext::new(None, &params);
    let got = eval(&Expr::Param(1), &ctx).unwrap();
    assert_eq!(got, Value::Str("hi".into()));
}

#[test]
fn parameter_zero_is_not_aliased_to_parameter_one() {
    let params = vec![SQLParam::Scalar(Value::Str("secret".into()))];
    let ctx = EvalContext::new(None, &params);
    assert!(matches!(
        eval(&Expr::Param(0), &ctx),
        Err(SQLError::MissingParam(0))
    ));
}

#[test]
fn array_constructor_creates_a_sql_array() {
    let ctx = EvalContext::new(None, &[]);
    let got = eval(
        &Expr::Array(vec![
            Expr::Literal(Value::Int(1)),
            Expr::Literal(Value::Int(2)),
        ]),
        &ctx,
    )
    .unwrap();
    assert_eq!(
        got,
        Value::Array(ArrayValue::try_new(vec![Value::Int(1), Value::Int(2)]).unwrap())
    );
}

#[test]
fn postgres_array_literals_preserve_nesting_quotes_and_nulls() {
    assert_eq!(
        parse_pg_array_literal(r#"{{"a,b",NULL},{"c\"d","NULL"}}"#).unwrap(),
        ArrayValue::try_new(vec![
            Value::List(vec![Value::Str("a,b".into()), Value::Null]),
            Value::List(vec![Value::Str("c\"d".into()), Value::Str("NULL".into()),]),
        ])
        .unwrap()
    );
}

#[test]
fn postgres_array_literals_reject_invalid_or_ragged_shapes() {
    for literal in ["{1,}", "{1", "{{1,2},{3}}", "{{1},2}", "{\"unterminated}"] {
        let error = parse_pg_array_literal(literal).unwrap_err();
        assert!(error.to_string().contains("malformed array literal"));
    }
}

#[test]
fn multidimensional_array_functions_use_every_dimension() {
    let matrix = Value::Array(
        ArrayValue::try_new(vec![
            Value::List(vec![Value::Int(1), Value::Int(2)]),
            Value::List(vec![Value::Int(3), Value::Int(4)]),
        ])
        .unwrap(),
    );
    assert_eq!(
        eval_scalar_function("array_length", &[matrix.clone(), Value::Int(1)]).unwrap(),
        Value::Int(2)
    );
    assert_eq!(
        eval_scalar_function("array_upper", &[matrix.clone(), Value::Int(2)]).unwrap(),
        Value::Int(2)
    );
    assert_eq!(
        eval_scalar_function("array_lower", &[matrix.clone(), Value::Int(2)]).unwrap(),
        Value::Int(1)
    );
    assert_eq!(
        eval_scalar_function("cardinality", &[matrix]).unwrap(),
        Value::Int(4)
    );
    assert_eq!(
        eval_scalar_function(
            "cardinality",
            &[Value::Array(ArrayValue::try_new(Vec::new()).unwrap())]
        )
        .unwrap(),
        Value::Int(0)
    );
}

#[test]
fn array_functions_reject_ragged_values_and_invalid_arity() {
    assert!(ArrayValue::try_new(vec![
        Value::List(vec![Value::Int(1)]),
        Value::List(vec![Value::Int(2), Value::Int(3)]),
    ])
    .is_none());
    assert!(eval_scalar_function("cardinality", &[]).is_err());
    assert!(eval_scalar_function("unnest", &[]).is_err());
}

#[test]
fn projected_row_lookup_evaluates_columns_without_a_result_map() {
    struct ProjectedRow {
        names: [&'static str; 2],
        values: [Value; 2],
    }

    impl RowLookup for ProjectedRow {
        fn column(&self, name: &str) -> Option<&Value> {
            self.names
                .iter()
                .position(|candidate| *candidate == name)
                .and_then(|index| self.values.get(index))
        }

        fn qualified_column(&self, _qualifier: &str, column: &str) -> Option<&Value> {
            self.column(column)
        }
    }

    let row = ProjectedRow {
        names: ["quantity", "status"],
        values: [Value::Int(7), Value::Str("O".into())],
    };
    let ctx = EvalContext::from_row_lookup(&row, &[]);

    assert_eq!(
        eval(&Expr::Column("quantity".into()), &ctx).unwrap(),
        Value::Int(7)
    );
    assert_eq!(
        eval(
            &Expr::QualifiedColumn {
                qualifier: "lineitem".into(),
                column: "status".into(),
            },
            &ctx,
        )
        .unwrap(),
        Value::Str("O".into())
    );
    assert_eq!(
        eval(
            &Expr::Binary {
                op: BinaryOp::Greater,
                lhs: Box::new(Expr::Column("quantity".into())),
                rhs: Box::new(Expr::Literal(Value::Int(5))),
            },
            &ctx,
        )
        .unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn named_result_rows_never_infer_qualified_identity_from_dotted_labels() {
    let row = ResultRow::from([("source.id".into(), Value::Int(7))]);
    assert_eq!(row.column("source.id"), Some(&Value::Int(7)));
    assert_eq!(row.column("id"), None);
    assert_eq!(row.qualified_column("source", "id"), None);
}

#[test]
fn value_to_vector_accepts_floats_and_ints() {
    let v = Value::List(vec![Value::Float(0.5), Value::Int(1), Value::Float(-1.5)]);
    let got = value_to_vector(&v).unwrap();
    assert_eq!(got, vec![0.5, 1.0, -1.5]);
}

#[test]
fn value_to_tensor_accepts_array_of_vectors() {
    let v = Value::List(vec![
        Value::List(vec![Value::Float(1.0), Value::Int(0)]),
        Value::List(vec![Value::Int(0), Value::Float(1.0)]),
    ]);
    let got = value_to_tensor(&v).unwrap();
    assert_eq!(got, vec![vec![1.0, 0.0], vec![0.0, 1.0]]);
}

#[test]
fn value_to_vector_rejects_non_finite_and_out_of_range_elements() {
    for value in [f64::NAN, f64::INFINITY, f64::MAX] {
        assert!(value_to_vector(&Value::List(vec![Value::Float(value)])).is_err());
    }
}

#[test]
fn json_conversion_preserves_non_finite_float_meaning() {
    assert_eq!(
        value_to_json(&Value::Float(f64::NAN)),
        serde_json::Value::String("NaN".into())
    );
    assert_eq!(
        value_to_json(&Value::Float(f64::INFINITY)),
        serde_json::Value::String("Infinity".into())
    );
    assert_eq!(
        value_to_json(&Value::Float(f64::NEG_INFINITY)),
        serde_json::Value::String("-Infinity".into())
    );
}

#[test]
fn integer_projection_rejects_float_saturation_boundaries() {
    for value in [
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        9_223_372_036_854_775_808.0,
    ] {
        assert!(to_i64(&Value::Float(value)).is_err(), "accepted {value}");
        assert!(cast_integer(&Value::Float(value), "bigint").is_err());
        assert_eq!(coerce_i64(&Value::Float(value)), None);
    }
    assert_eq!(
        to_i64(&Value::Float(-9_223_372_036_854_775_808.0)).unwrap(),
        i64::MIN
    );
}

#[test]
fn numeric_comparison_preserves_large_integer_and_nan_ordering() {
    let rounded = Value::Float(9_007_199_254_740_992.0);
    let next_integer = Value::Int(9_007_199_254_740_993);
    assert_eq!(
        eval_comparison_op(BinaryOp::Equal, &rounded, &next_integer).unwrap(),
        Value::Bool(false)
    );
    assert_eq!(
        eval_comparison_op(BinaryOp::Less, &rounded, &next_integer).unwrap(),
        Value::Bool(true)
    );

    let nan = Value::Float(f64::NAN);
    assert_eq!(
        eval_comparison_op(BinaryOp::Equal, &nan, &Value::Float(f64::NAN)).unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        eval_comparison_op(BinaryOp::Greater, &nan, &Value::Float(f64::INFINITY)).unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn integer_builtins_report_overflow_instead_of_panicking_or_saturating() {
    assert!(eval_scalar_function("abs", &[Value::Int(i64::MIN)]).is_err());
    assert!(eval_scalar_function("mod", &[Value::Int(i64::MIN), Value::Int(-1)]).is_err());
    assert!(eval_scalar_function("div", &[Value::Int(i64::MIN), Value::Int(-1)]).is_err());
    assert!(eval_scalar_function("gcd", &[Value::Int(i64::MIN), Value::Int(0)]).is_err());
    assert!(eval_scalar_function("lcm", &[Value::Int(i64::MAX), Value::Int(2)]).is_err());
    assert!(eval_scalar_function("chr", &[Value::Int(-1)]).is_err());
}

#[test]
fn temporal_builtins_reject_narrowing_and_arithmetic_overflow() {
    assert!(eval_scalar_function("to_timestamp", &[Value::Float(f64::MAX)]).is_err());
    assert!(eval_scalar_function(
        "make_timestamp",
        &[
            Value::Int(i64::MAX),
            Value::Int(1),
            Value::Int(1),
            Value::Int(0),
            Value::Int(0),
            Value::Int(0),
        ],
    )
    .is_err());
    assert!(eval_scalar_function(
        "make_timestamp",
        &[
            Value::Int(2026),
            Value::Int(1),
            Value::Int(1),
            Value::Int(0),
            Value::Int(0),
            Value::Float(f64::NAN),
        ],
    )
    .is_err());
    assert!(
        eval_scalar_function("make_interval", &[Value::Int(i64::MAX), Value::Int(0)],).is_err()
    );
    assert!(eval_scalar_function(
        "justify_hours",
        &[Value::Temporal(TemporalValue::Interval {
            months: 0,
            days: i32::MAX,
            micros: 86_400_000_000,
        })],
    )
    .is_err());
    assert!(eval_binary_values(
        BinaryOp::Multiply,
        &Value::Temporal(TemporalValue::Interval {
            months: 1,
            days: 0,
            micros: 0,
        }),
        &Value::Float(f64::INFINITY),
    )
    .is_err());
}

#[test]
fn collection_and_string_sizes_fail_without_wrapping_or_panicking() {
    assert!(
        eval_scalar_function("lpad", &[Value::Str("x".into()), Value::Int(i64::MAX)],).is_err()
    );
    assert!(
        eval_scalar_function("repeat", &[Value::Str("x".into()), Value::Int(i64::MAX)],).is_err()
    );
    assert!(eval_scalar_function(
        "array_fill",
        &[
            Value::Int(1),
            Value::Array(ArrayValue::try_new(vec![Value::Int(i64::MAX)]).unwrap()),
        ],
    )
    .is_err());

    let values = Value::Array(ArrayValue::try_new(vec![Value::Int(1), Value::Int(2)]).unwrap());
    assert!(eval_scalar_function("trim_array", &[values.clone(), Value::Int(i64::MAX)]).is_err());
    assert!(eval_scalar_function("array_sample", &[values.clone(), Value::Int(i64::MAX)]).is_err());
    assert_eq!(
        eval_scalar_function("__subscript", &[values.clone(), Value::Int(i64::MAX)]).unwrap(),
        Value::Null
    );
    assert_eq!(
        eval_scalar_function(
            "__slice",
            &[values, Value::Int(i64::MAX), Value::Int(i64::MAX)],
        )
        .unwrap(),
        Value::Array(ArrayValue::try_new(Vec::new()).unwrap())
    );
    assert_eq!(
        eval_scalar_function(
            "overlay",
            &[
                Value::Str("abc".into()),
                Value::Str("x".into()),
                Value::Int(i64::MAX),
                Value::Int(i64::MAX),
            ],
        )
        .unwrap(),
        Value::Str("abcx".into())
    );
}

#[test]
fn builtin_strictness_matches_null_call_semantics() {
    assert_eq!(builtin_scalar_function_strictness("abs", 1), Some(true));
    assert_eq!(
        builtin_scalar_function_strictness("pg_catalog.upper", 1),
        Some(true)
    );
    assert_eq!(
        builtin_scalar_function_strictness("coalesce", 2),
        Some(false)
    );
    assert_eq!(
        builtin_scalar_function_strictness("array_to_string", 2),
        Some(true)
    );
    assert_eq!(
        builtin_scalar_function_strictness("array_to_string", 3),
        Some(false)
    );
    assert_eq!(
        builtin_scalar_function_strictness("application_fn", 1),
        None
    );
    assert_eq!(builtin_scalar_function_strictness("to_bin", 1), Some(true));
    assert_eq!(builtin_scalar_function_strictness("to_oct", 1), Some(true));
    assert_eq!(
        eval_scalar_function("abs", &[Value::Null]).unwrap(),
        Value::Null
    );
    assert_eq!(
        eval_scalar_function("to_json", &[Value::Null]).unwrap(),
        Value::Null
    );
}
