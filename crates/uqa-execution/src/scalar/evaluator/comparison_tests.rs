//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};
use uqa_sql::{ResultRow, RowSchema};

#[test]
fn record_columns_and_row_constructors_select_distinct_null_rules() {
    let row = ResultRow::from([
        ("l".into(), Value::Row(vec![Value::Null].into())),
        ("r".into(), Value::Row(vec![Value::Null].into())),
    ]);
    let schema = RowSchema::with_types(
        vec!["l".into(), "r".into()],
        vec![Some(uqa_sql::ColumnType::Record); 2],
    );
    let budget = MemoryBudget::new(16 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for (operator, record_result) in [
        ("=", true),
        ("<>", false),
        ("<", false),
        ("<=", true),
        (">", false),
        (">=", true),
    ] {
        for (source, expected) in [
            (format!("SELECT l {operator} r"), Value::Bool(record_result)),
            (
                format!("SELECT ROW(NULL::integer) {operator} ROW(NULL::integer)"),
                Value::Null,
            ),
            (
                format!("SELECT ROW(NULL::integer)::record {operator} ROW(NULL::integer)::record"),
                Value::Bool(record_result),
            ),
        ] {
            let uqa_sql::Statement::Select(mut query) =
                uqa_sql::compile(&source).unwrap().remove(0)
            else {
                panic!("SELECT comparison")
            };
            let scalar =
                uqa_sql::plan::ExpressionPlan::lower(query.projections.remove(0).expr).scalar;
            let bound = uqa_sql::bind_type_introspection(scalar, &schema, &[]);
            assert_eq!(
                eval_scalar(&bound, &ScalarEvalContext::from_row_lookup(&row, &[])).unwrap(),
                expected,
                "{source}"
            );
            assert_eq!(
                *eval_generated_scalar_with_control(&bound, &row, &control).unwrap(),
                expected,
                "{source}"
            );
            assert_eq!(budget.used(), 0);
        }
    }
}

#[test]
fn simple_case_uses_record_equality_in_ordinary_and_generated_evaluation() {
    let row = ResultRow::from([
        (
            "l".into(),
            Value::Row(vec![Value::Int(1), Value::Null].into()),
        ),
        (
            "r".into(),
            Value::Row(vec![Value::Int(1), Value::Null].into()),
        ),
    ]);
    let schema = RowSchema::with_types(
        vec!["l".into(), "r".into()],
        vec![Some(uqa_sql::ColumnType::Record); 2],
    );
    let budget = MemoryBudget::new(16 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let uqa_sql::Statement::Select(mut query) =
        uqa_sql::compile("SELECT CASE l WHEN r THEN 1 ELSE 0 END")
            .unwrap()
            .remove(0)
    else {
        panic!("SELECT expression");
    };
    let scalar = uqa_sql::plan::ExpressionPlan::lower(query.projections.remove(0).expr).scalar;
    let bound = uqa_sql::bind_type_introspection(scalar, &schema, &[]);
    assert_eq!(
        eval_scalar(&bound, &ScalarEvalContext::from_row_lookup(&row, &[])).unwrap(),
        Value::Int(1)
    );
    assert_eq!(
        *eval_generated_scalar_with_control(&bound, &row, &control).unwrap(),
        Value::Int(1)
    );
    assert_eq!(budget.used(), 0);
}

#[derive(serde::Deserialize)]
struct Oracle {
    postgresql: String,
    image: String,
    operators: Vec<String>,
    cases: Vec<Case>,
}

#[derive(serde::Deserialize)]
struct Case {
    left: String,
    right: String,
    r#type: Option<String>,
    values: Option<Vec<Option<bool>>>,
    sqlstate: Option<String>,
}

#[test]
fn numeric_comparisons_match_postgresql_in_ordinary_and_generated_evaluation() {
    let oracle: Oracle = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../uqa-sql/src/expr/binary/comparison/pg18.json"
    )))
    .unwrap();
    assert!(oracle.postgresql.starts_with("PostgreSQL 18."));
    assert!(oracle.image.starts_with("sha256:"));
    assert_eq!(oracle.operators, ["=", "<>", "<", "<=", ">", ">="]);
    let row = ResultRow::new();
    let schema = RowSchema::default();
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for case in oracle.cases {
        for (index, operator) in oracle.operators.iter().enumerate() {
            let source = format!("SELECT ({}) {operator} ({})", case.left, case.right);
            let uqa_sql::Statement::Select(mut query) =
                uqa_sql::compile(&source).unwrap().remove(0)
            else {
                panic!("SELECT comparison")
            };
            let scalar =
                uqa_sql::plan::ExpressionPlan::lower(query.projections.remove(0).expr).scalar;
            for controlled in [false, true] {
                let result = uqa_sql::scalar_type(&scalar, &schema, &[]).and_then(|ty| {
                    if case.sqlstate.is_none() {
                        assert_eq!(
                            ty.as_ref().map(uqa_sql::ColumnType::sql_name),
                            case.r#type,
                            "{source}"
                        );
                    }
                    let bound = uqa_sql::bind_type_introspection(scalar.clone(), &schema, &[]);
                    if controlled {
                        eval_generated_scalar_with_control(&bound, &row, &control)
                            .map(|output| (*output).clone())
                    } else {
                        eval_scalar(&bound, &ScalarEvalContext::from_row_lookup(&row, &[]))
                    }
                });
                if let Some(expected) = &case.sqlstate {
                    assert_eq!(
                        result.unwrap_err().sqlstate(),
                        Some(expected.as_str()),
                        "{source}; controlled={controlled}"
                    );
                } else {
                    let expected = case.values.as_ref().unwrap()[index]
                        .map(Value::Bool)
                        .unwrap_or(Value::Null);
                    assert_eq!(
                        result.unwrap(),
                        expected,
                        "{source}; controlled={controlled}"
                    );
                }
                assert_eq!(budget.used(), 0, "{source}");
            }
        }
    }
}
