//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, ArrayValue, CancellationToken};
use uqa_sql::{ColumnType, ResultRow};

fn expression(source: &str, schema: &crate::RowSchema) -> ScalarExpr {
    let uqa_sql::Statement::Select(query) = uqa_sql::compile(&format!("SELECT {source}"))
        .unwrap()
        .remove(0)
    else {
        panic!("SELECT");
    };
    let expression = uqa_sql::plan::ExpressionPlan::lower(query.projections[0].expr.clone()).scalar;
    uqa_sql::bind_type_introspection(expression, schema, &[])
}

fn arithmetic_oracle() -> serde_json::Value {
    let oracle: serde_json::Value =
        serde_json::from_str(include_str!("pg18_arithmetic.json")).unwrap();
    assert!(oracle["postgresql"]
        .as_str()
        .unwrap()
        .starts_with("PostgreSQL 18."));
    assert!(oracle["image"].as_str().unwrap().starts_with("sha256:"));
    oracle
}

#[test]
fn nested_numeric_arithmetic_keeps_exact_results_with_a_small_allowance() {
    let oracle = arithmetic_oracle();
    let numeric = ColumnType::Numeric {
        precision: None,
        scale: None,
    };
    let schema = crate::RowSchema::with_types(
        ["price", "discount", "tax", "cost", "quantity"]
            .map(str::to_string)
            .to_vec(),
        vec![Some(numeric); 5],
    );
    let row = ResultRow::from(
        [
            ("price".into(), "100.00"),
            ("discount".into(), "0.07"),
            ("tax".into(), "0.20"),
            ("cost".into(), "1.25"),
            ("quantity".into(), "3.00"),
        ]
        .map(|(name, value)| {
            (
                name,
                Value::Decimal(uqa_core::DecimalValue::parse(value).unwrap()),
            )
        }),
    );
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for (source, key) in [
        ("price * (1 - discount) * (1 + tax)", "charge"),
        ("price * (1 - discount) - cost * quantity", "profit"),
    ] {
        let expected = oracle[key].as_str().unwrap();
        let expression = expression(source, &schema);
        let context = ScalarEvalContext::from_row_lookup(&row, &[]).with_row_schema(&schema);
        let output = eval_scalar_inner(&expression, &context, &control).unwrap();
        let Value::Decimal(value) = &*output else {
            panic!("numeric result for {source}");
        };
        assert_eq!(value.to_sql_string(), expected, "{source}");
        assert_eq!(budget.used(), output.reserved_bytes(), "{source}");
        drop(output);
        assert_eq!(budget.used(), 0, "{source}");
        assert_eq!(
            eval_scalar(&expression, &context).unwrap(),
            Value::Decimal(uqa_core::DecimalValue::parse(expected).unwrap()),
            "ordinary {source}"
        );
    }
}

#[test]
fn integer_arithmetic_retains_declared_width_errors_and_null_results() {
    let oracle = arithmetic_oracle();
    for (ty, value, name) in [
        (ColumnType::SmallInteger, i64::from(i16::MAX), "int2"),
        (ColumnType::Integer, i64::from(i32::MAX), "int4"),
        (ColumnType::BigInteger, i64::MAX, "int8"),
    ] {
        let schema = crate::RowSchema::with_types(
            vec!["v".into(), "w".into()],
            vec![Some(ty.clone()), Some(ty)],
        );
        let mut row =
            ResultRow::from([("v".into(), Value::Int(value)), ("w".into(), Value::Int(1))]);
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        for (source, key) in [
            ("v + w", format!("{name}_overflow")),
            ("(v + w) - w", format!("nested_{name}_overflow")),
        ] {
            let expression = expression(source, &schema);
            let context = ScalarEvalContext::from_row_lookup(&row, &[]).with_row_schema(&schema);
            assert_eq!(
                eval_scalar_inner(&expression, &context, &control)
                    .unwrap_err()
                    .sqlstate(),
                oracle[key].as_str(),
                "{source}"
            );
            assert_eq!(budget.used(), 0);
        }
        row.insert("w".into(), Value::Null);
        let expression = expression("v + w", &schema);
        let context = ScalarEvalContext::from_row_lookup(&row, &[]).with_row_schema(&schema);
        let output = eval_scalar_inner(&expression, &context, &control).unwrap();
        assert_eq!(oracle[format!("{name}_null")], true);
        assert_eq!(*output, Value::Null);
        drop(output);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn generated_scalar_results_keep_the_original_allowance_through_shared_evaluation() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let schema = crate::RowSchema::with_types(
        vec!["label".into(), "n".into()],
        vec![Some(ColumnType::Text), Some(ColumnType::SmallInteger)],
    );
    let row = ResultRow::from([
        ("label".into(), Value::Str("ab ab".into())),
        ("n".into(), Value::Int(7)),
    ]);
    for (source, expected) in [
        (
            "upper(label) || repeat('!', 3)",
            Value::Str("AB AB!!!".into()),
        ),
        ("n + 2", Value::Int(9)),
        ("16777216::real + 1::real", Value::Float(16_777_216.0)),
        (
            "CASE WHEN n > 0 THEN array_append(ARRAY[n, n + 1], n + 2) ELSE ARRAY[0] END",
            Value::Array(
                ArrayValue::try_new(vec![Value::Int(7), Value::Int(8), Value::Int(9)]).unwrap(),
            ),
        ),
        (
            "coalesce('present', repeat('x', 9223372036854775807))",
            Value::Str("present".into()),
        ),
        (
            "'{\"field\":\"selected\"}'::jsonb ->> 'field'",
            Value::Str("selected".into()),
        ),
        (
            r"regexp_replace(label, '(ab)', '<\1>', 'g')",
            Value::Str("<ab> <ab>".into()),
        ),
        (
            "regexp_replace(flags => 'g', replacement => 'X', string => label, pattern => 'ab')",
            Value::Str("X X".into()),
        ),
        (
            "'[1,4)'::int4range @> '[2,3)'::int4range",
            Value::Bool(true),
        ),
    ] {
        let expression = expression(source, &schema);
        let output = eval_generated_scalar_with_control(&expression, &row, &control).unwrap();
        assert_eq!(*output, expected, "{source}");
        assert_eq!(budget.used(), output.reserved_bytes(), "{source}");
        assert_eq!(
            eval_scalar(&expression, &ScalarEvalContext::from_row_lookup(&row, &[])).unwrap(),
            expected,
            "ordinary {source}"
        );
        drop(output);
        assert_eq!(budget.used(), 0);
    }
    assert_eq!(row["label"], Value::Str("ab ab".into()));
}

#[test]
fn generated_scalar_quota_failure_releases_partial_array_and_preserves_previous_output() {
    let budget = MemoryBudget::new(8192);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let row = ResultRow::new();
    let schema = crate::RowSchema::default();
    let retained_expression = expression("repeat('p', 64)", &schema);
    let retained =
        eval_generated_scalar_with_control(&retained_expression, &row, &control).unwrap();
    let used = budget.used();
    let failure = expression(
        "ARRAY[repeat('first', 8), repeat('large', 100000)]",
        &schema,
    );
    assert_eq!(
        eval_generated_scalar_with_control(&failure, &row, &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(budget.used(), used);
    assert_eq!(*retained, Value::Str("p".repeat(64)));
    drop(retained);
    assert_eq!(budget.used(), 0);
}

#[test]
fn generated_scalar_checks_both_cancellations_without_replacing_held_results() {
    let budget = MemoryBudget::new(8192);
    let row = ResultRow::new();
    let expression = expression("repeat('held', 8)", &crate::RowSchema::default());
    for cancel_original in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let held = eval_generated_scalar_with_control(&expression, &row, &control).unwrap();
        let used = budget.used();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert_eq!(
            eval_generated_scalar_with_control(&expression, &row, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), used);
        assert_eq!(*held, Value::Str("held".repeat(8)));
        drop(held);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn generated_lazy_branches_and_typed_numeric_errors_keep_existing_precedence() {
    let budget = MemoryBudget::new(8192);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let row = ResultRow::new();
    let schema = crate::RowSchema::default();
    for source in ["false AND 1 / 0 = 1", "true OR 1 / 0 = 1"] {
        let value =
            eval_generated_scalar_with_control(&expression(source, &schema), &row, &control)
                .unwrap();
        assert_eq!(*value, Value::Bool(source.starts_with("true")));
        drop(value);
        assert_eq!(budget.used(), 0);
    }
    assert_eq!(
        eval_generated_scalar_with_control(&expression("1 / 0", &schema), &row, &control)
            .unwrap_err()
            .sqlstate(),
        Some("22012")
    );
    assert_eq!(budget.used(), 0);
}

#[test]
fn generated_range_element_evaluation_preserves_the_shared_owner_outcome() {
    let budget = MemoryBudget::new(8192);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let row = ResultRow::new();
    let expression = expression("'[1,4)'::int4range @> 2", &crate::RowSchema::default());
    let ordinary = eval_scalar(&expression, &ScalarEvalContext::from_row_lookup(&row, &[]));
    let generated = eval_generated_scalar_with_control(&expression, &row, &control);
    match (ordinary, generated) {
        (Ok(expected), Ok(actual)) => assert_eq!(expected, *actual),
        (Err(expected), Err(actual)) => {
            assert_eq!(expected.sqlstate(), actual.sqlstate());
            assert_eq!(expected.to_string(), actual.to_string());
        }
        (ordinary, generated) => panic!("different outcomes: {ordinary:?}, {generated:?}"),
    }
    assert_eq!(budget.used(), 0);
}
