//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn bound_call(name: &str, args: Vec<ScalarExpr>) -> ScalarExpr {
    ScalarExpr::Func {
        order_syntax: uqa_sql::ast::FunctionOrderSyntax::Ordinary,
        name: name.into(),
        binding: Some(FunctionBinding {
            name: name.into(),
            argument_types: Vec::new(),
            builtin: true,
            object_id: None,
            dispatch: None,
            invocation: None,
            resolution_error: None,
        }),
        args,
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    }
}

#[test]
fn folds_value_builtins_after_binding_without_evaluating_stateful_or_set_calls() {
    let mut repeated = bound_call(
        "pg_catalog.repeat",
        vec![
            ScalarExpr::Literal(Value::Str("x".into())),
            ScalarExpr::Literal(Value::Int(3)),
        ],
    );
    if let ScalarExpr::Func {
        binding: Some(binding),
        ..
    } = &mut repeated
    {
        binding.argument_types = vec!["text".into(), "integer".into()];
    }
    let folded =
        fold_literal_expression(repeated, uqa_execution::scalar::eval_constant_scalar).unwrap();
    assert_eq!(literal_value(&folded), Some(&Value::Str("xxx".into())));

    for name in [
        "nextval",
        "current_setting",
        "concat",
        "sum",
        "pg_catalog.sum",
        "generate_series",
    ] {
        let call = bound_call(name, vec![ScalarExpr::Literal(Value::Int(1))]);
        let unchanged = fold_literal_expression(call.clone(), |_| {
            panic!("stateful or set call must not execute during planning")
        })
        .unwrap();
        assert_eq!(unchanged, call, "{name}");
    }
    let mut user_call = bound_call("repeat", vec![ScalarExpr::Literal(Value::Int(1))]);
    if let ScalarExpr::Func {
        binding: Some(binding),
        ..
    } = &mut user_call
    {
        binding.builtin = false;
    }
    assert!(!is_constant(&user_call));
}

#[test]
fn named_arguments_keep_their_call_context_during_constant_planning() {
    let uqa_sql::Statement::Select(mut select) = uqa_sql::compile(
        "SELECT json_strip_nulls(strip_in_arrays => true, target => '{\"keep\":1,\"drop\":null}'::json)",
    ).unwrap().remove(0) else {
        panic!("SELECT expression");
    };
    let mut expression =
        uqa_sql::plan::ExpressionPlan::lower(select.projections.remove(0).expr).scalar;
    let before = uqa_execution::scalar::eval_constant_scalar(&expression).unwrap();
    crate::optimizer::optimize_scalar_expression(
        &mut expression,
        &crate::OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar),
    )
    .unwrap();
    assert_eq!(
        uqa_execution::scalar::eval_constant_scalar(&expression).unwrap(),
        before
    );
}

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
                    .map_or(Value::Null, Value::Bool);
                assert_eq!(literal_value(&folded), Some(&expected), "{sql}");
                assert_eq!(
                    scalar_type(&folded, &RowSchema::default(), &[]).unwrap(),
                    Some(ColumnType::Boolean)
                );
            }
        }
    }
}
