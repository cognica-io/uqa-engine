//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};
use uqa_sql::{ResultRow, RowSchema};

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
