//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn constant_numeric_comparisons_match_postgresql_before_replacing_the_expression() {
    let oracle: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../uqa-sql/src/expr/binary/comparison/pg18.json"
    )))
    .unwrap();
    for case in oracle["cases"].as_array().unwrap() {
        for (index, operator) in oracle["operators"].as_array().unwrap().iter().enumerate() {
            let sql = format!(
                "SELECT ({}) {} ({})",
                case["left"].as_str().unwrap(),
                operator.as_str().unwrap(),
                case["right"].as_str().unwrap()
            );
            let uqa_sql::Statement::Select(mut select) = uqa_sql::compile(&sql).unwrap().remove(0)
            else {
                panic!("SELECT comparison")
            };
            let expression =
                uqa_sql::plan::ExpressionPlan::lower(select.projections.remove(0).expr).scalar;
            let result =
                fold_literal_expression(expression, uqa_execution::scalar::eval_constant_scalar);
            if let Some(expected) = case["sqlstate"].as_str() {
                assert_eq!(result.unwrap_err().sqlstate(), Some(expected), "{sql}");
            } else {
                let folded = result.unwrap();
                let expected = case["values"][index]
                    .as_bool()
                    .map(Value::Bool)
                    .unwrap_or(Value::Null);
                assert_eq!(literal_value(&folded), Some(&expected), "{sql}");
                assert_eq!(
                    scalar_type(&folded, &RowSchema::default(), &[]).unwrap(),
                    Some(ColumnType::Boolean)
                );
            }
        }
    }
}
