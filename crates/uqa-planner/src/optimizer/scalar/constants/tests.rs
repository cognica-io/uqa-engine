//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn bound_call(name: &str, args: Vec<ScalarExpr>) -> ScalarExpr {
    let selected = match name {
        "lower" | "upper" | "replace" => uqa_sql::resolve_fixed_builtin_call(
            name,
            None,
            &vec![None; args.len()],
            &vec![Some(ColumnType::Text); args.len()],
            false,
            None,
        )
        .unwrap()
        .map(|call| call.selected.binding),
        _ => None,
    };
    ScalarExpr::Func {
        order_syntax: uqa_sql::ast::FunctionOrderSyntax::Ordinary,
        name: name.into(),
        binding: selected.or_else(|| {
            Some(FunctionBinding {
                name: name.into(),
                argument_types: Vec::new(),
                builtin: true,
                object_id: None,
                dispatch: None,
                invocation: None,
                resolution_error: None,
            })
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

#[derive(Debug)]
struct DeniedBuiltin;
impl uqa_sql::catalog::security::builtin_routines::BuiltinRoutineExecution for DeniedBuiltin {
    fn require_execute(&self, _: &FunctionBinding) -> Result<(), SQLError> {
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: "permission denied for function lower".into(),
        })
    }
}

#[test]
fn constant_evaluation_checks_selected_permission_after_strict_null_simplification() {
    let nonnull = bound_call(
        "lower",
        vec![ScalarExpr::Literal(Value::Str("HELLO".into()))],
    );
    let error = fold_authorized_literal(
        nonnull,
        |_| panic!("denied function ran"),
        Some(&DeniedBuiltin),
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42501"));
    let null = bound_call(
        "lower",
        vec![ScalarExpr::TypedLiteral {
            value: Value::Null,
            ty: "text".into(),
            bound_type: Some(ColumnType::Text),
            parameter_index: None,
        }],
    );
    let output = fold_authorized_literal(
        null,
        |_| panic!("strict NULL function ran"),
        Some(&DeniedBuiltin),
    )
    .unwrap();
    assert_eq!(literal_value(&output), Some(&Value::Null));
}

#[test]
fn eliminated_conditional_calls_do_not_require_execute_permission() {
    let mut expression = ScalarExpr::Case {
        base: None,
        when: vec![(
            ScalarExpr::Literal(Value::Bool(false)),
            bound_call(
                "lower",
                vec![ScalarExpr::Literal(Value::Str("HELLO".into()))],
            ),
        )],
        else_branch: Some(Box::new(ScalarExpr::Literal(Value::Str("ok".into())))),
    };
    let mut config = crate::OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar);
    config.builtin_permissions = Some(std::sync::Arc::new(DeniedBuiltin));
    crate::optimizer::optimize_scalar_expression(&mut expression, &config).unwrap();
    assert_eq!(literal_value(&expression), Some(&Value::Str("ok".into())));
}

#[test]
fn strict_null_simplification_does_not_evaluate_nonconstant_siblings() {
    let mut expression = bound_call(
        "replace",
        vec![
            ScalarExpr::TypedLiteral {
                value: Value::Null,
                ty: "text".into(),
                bound_type: Some(ColumnType::Text),
                parameter_index: None,
            },
            ScalarExpr::Column("v".into()),
            ScalarExpr::Literal(Value::Str("x".into())),
        ],
    );
    if let ScalarExpr::Func {
        binding: Some(binding),
        ..
    } = &mut expression
    {
        binding.argument_types = vec!["text".into(); 3];
    }
    let folded = fold_authorized_literal(
        expression,
        |_| panic!("strict NULL evaluated a sibling"),
        Some(&DeniedBuiltin),
    )
    .unwrap();
    assert_eq!(literal_value(&folded), Some(&Value::Null));
    assert_eq!(
        scalar_type(&folded, &RowSchema::default(), &[]).unwrap(),
        Some(ColumnType::Text)
    );
}

#[test]
fn unreachable_scalar_subqueries_keep_types_without_constant_evaluation() {
    for (sql, denied) in [
        (
            "SELECT CASE WHEN false THEN (SELECT lower('HELLO')) ELSE 'ok' END",
            false,
        ),
        ("VALUES (CASE WHEN false THEN (SELECT lower('HELLO')) ELSE 'ok' END)", false),
        ("INSERT INTO target VALUES (CASE WHEN false THEN (SELECT lower('HELLO')) ELSE 'ok' END)", false),
        ("UPDATE target SET value=CASE WHEN false THEN (SELECT lower('HELLO')) ELSE 'ok' END", false),
        ("DELETE FROM target RETURNING CASE WHEN false THEN (SELECT lower('HELLO')) ELSE 'ok' END", false),
        ("MERGE INTO target t USING source s ON t.id=s.id WHEN MATCHED THEN UPDATE SET value=CASE WHEN false THEN (SELECT lower('HELLO')) ELSE 'ok' END", false),
        (
            "SELECT CASE WHEN random() < 0 THEN (SELECT lower('HELLO')) ELSE 'ok' END",
            true,
        ),
    ] {
        let mut plan = crate::UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0));
        plan.rewrite_scalar_expressions(&mut |expression| {
            if let ScalarExpr::Func { name, binding, .. } = expression {
                if name == "lower" {
                    *binding = uqa_sql::resolve_fixed_builtin_call(
                        name,
                        None,
                        &[None],
                        &[Some(ColumnType::Text)],
                        false,
                        None,
                    )
                    .unwrap()
                    .map(|call| call.selected.binding);
                }
            }
        });
        let mut config = crate::OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar);
        config.builtin_permissions = Some(std::sync::Arc::new(DeniedBuiltin));
        let result = crate::optimizer::optimize(plan, &config);
        assert_eq!(result.is_err(), denied, "{sql}: {result:?}");
    }
}

#[test]
fn typed_inline_results_keep_catalog_and_session_dependent_output_at_runtime() {
    let mood = ColumnType::Enum(uqa_sql::ast::EnumTypeReference {
        schema: "public".into(),
        name: "mood".into(),
        oid: 20_000,
        array_oid: 20_001,
    });
    let values = [
        (
            mood,
            Value::Enum(uqa_core::EnumValue::new(
                20_000,
                uqa_core::EnumLabelKey::from_bytes(vec![1]).unwrap(),
            )),
        ),
        (
            ColumnType::TimestampTz,
            Value::Temporal(uqa_core::TemporalValue::TimestampTz { micros: 0 }),
        ),
    ];
    for (ty, value) in values {
        let expression = ScalarExpr::Cast {
            expr: Box::new(ScalarExpr::TypedLiteral {
                value,
                ty: ty.catalog_name(),
                bound_type: Some(ty),
                parameter_index: None,
            }),
            ty: "text".into(),
        };
        let retained = fold_literal_expression(expression.clone(), |_| {
            panic!("a stable output function cannot run in the catalog-free constant evaluator")
        })
        .unwrap();
        assert_eq!(retained, expression);
    }
    let integer = ScalarExpr::Cast {
        expr: Box::new(ScalarExpr::TypedLiteral {
            value: Value::Int(7),
            ty: "integer".into(),
            bound_type: Some(ColumnType::Integer),
            parameter_index: None,
        }),
        ty: "text".into(),
    };
    let folded =
        fold_literal_expression(integer, uqa_execution::scalar::eval_constant_scalar).unwrap();
    assert_eq!(literal_value(&folded), Some(&Value::Str("7".into())));
}

#[test]
fn analyzed_conditionals_discard_unreachable_mutable_calls() {
    let mut config = crate::OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar);
    config.coerced_conditionals = true;
    for sql in [
        "SELECT CASE WHEN true THEN v ELSE random() END",
        "SELECT CASE WHEN false THEN random() ELSE v END",
        "SELECT CASE WHEN false THEN random() END",
        "SELECT coalesce(1.0,random())",
    ] {
        let uqa_sql::Statement::Select(query) = uqa_sql::compile(sql).unwrap().remove(0) else {
            panic!("SELECT")
        };
        let mut expression =
            uqa_sql::plan::ExpressionPlan::lower(query.projections[0].expr.clone()).scalar;
        crate::optimizer::optimize_scalar_expression(&mut expression, &config).unwrap();
        expression.visit(&mut |node| {
            assert!(
                !matches!(node, ScalarExpr::Func {name, ..} if name == "random"),
                "{sql}: {expression:?}"
            );
        });
    }
}

#[test]
fn null_casts_to_temporal_types_fold_without_calling_input_functions() {
    for ty in ["date", "time", "timestamp", "timestamptz", "interval"] {
        let expression = ScalarExpr::Cast {
            expr: Box::new(ScalarExpr::Literal(Value::Null)),
            ty: ty.into(),
        };
        let result =
            fold_authorized_literal(expression, |_| panic!("a NULL cast evaluated"), None).unwrap();
        assert_eq!(literal_value(&result), Some(&Value::Null));
        assert_eq!(
            uqa_sql::scalar_type(&result, &RowSchema::default(), &[]).unwrap(),
            Some(ColumnType::from_sql_name(ty).unwrap())
        );
    }
}
