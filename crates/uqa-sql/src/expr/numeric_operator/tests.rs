//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::FunctionDispatch;
use crate::expr::EngineHook;

fn expression(sql: &str) -> Expr {
    let crate::Statement::Select(mut query) =
        crate::compile(&format!("SELECT {sql}")).unwrap().remove(0)
    else {
        panic!("SELECT expression")
    };
    query.projections.remove(0).expr
}

#[test]
fn numeric_operators_match_postgresql_types_values_and_errors() {
    #[derive(serde::Deserialize)]
    struct Case {
        expression: String,
        r#type: Option<String>,
        value: Option<String>,
        sqlstate: Option<String>,
    }
    let cases: Vec<Case> = serde_json::from_str(include_str!("tests/values.json")).unwrap();
    for case in cases {
        let expr = expression(&case.expression);
        let scalar = crate::plan::ExpressionPlan::lower(expr.clone()).scalar;
        let result_type = crate::scalar_type(&scalar, &RowSchema::default(), &[]);
        let selected_type = result_type
            .as_ref()
            .ok()
            .and_then(Option::as_ref)
            .map(ColumnType::sql_name);
        let result = result_type.and_then(|_| eval(&expr, &EvalContext::new(None, &[])));
        if let Some(state) = case.sqlstate {
            assert_eq!(
                result.unwrap_err().sqlstate(),
                Some(state.as_str()),
                "{}",
                case.expression
            );
        } else {
            let actual = result.unwrap_or_else(|error| panic!("{}: {error}", case.expression));
            assert_eq!(selected_type, case.r#type, "{}", case.expression);
            let actual = (!matches!(actual, Value::Null))
                .then(|| crate::expr::value_to_string(&actual).unwrap());
            assert_eq!(actual, case.value, "{}", case.expression);
        }
    }
}

#[test]
fn numeric_function_binding_selects_postgresql_signatures() {
    let schema = RowSchema::default();
    for (sql, result, arguments) in [
        (
            "mod(7::smallint,3::smallint)",
            "smallint",
            vec!["smallint", "smallint"],
        ),
        ("mod(7,3)", "integer", vec!["integer", "integer"]),
        ("mod(7::bigint,3)", "bigint", vec!["bigint", "bigint"]),
        ("mod(7::numeric,3)", "numeric", vec!["numeric", "numeric"]),
        (
            "power(2,3)",
            "double precision",
            vec!["double precision", "double precision"],
        ),
        ("power(2::numeric,3)", "numeric", vec!["numeric", "numeric"]),
        ("pow(2::numeric,3)", "numeric", vec!["numeric", "numeric"]),
        ("sqrt(9)", "double precision", vec!["double precision"]),
        ("sqrt(9::numeric)", "numeric", vec!["numeric"]),
        (
            "cbrt(8::numeric)",
            "double precision",
            vec!["double precision"],
        ),
    ] {
        let scalar = crate::plan::ExpressionPlan::lower(expression(sql)).scalar;
        assert_eq!(
            crate::scalar_type(&scalar, &schema, &[])
                .unwrap()
                .unwrap()
                .sql_name(),
            result,
            "{sql}"
        );
        let bound = crate::bind_type_introspection(scalar, &schema, &[]);
        let crate::ScalarExpr::Func {
            binding: Some(binding),
            ..
        } = bound
        else {
            panic!("{sql} has no selected function")
        };
        assert_eq!(binding.argument_types, arguments, "{sql}");
        assert!(binding.builtin);
        assert!(binding.dispatch.is_none());
    }
    for sql in [
        "mod(7::float8,3)",
        "power(true,3)",
        "sqrt('9'::text)",
        "cbrt(true)",
    ] {
        let scalar = crate::plan::ExpressionPlan::lower(expression(sql)).scalar;
        assert_eq!(
            crate::scalar_type(&scalar, &schema, &[])
                .unwrap_err()
                .sqlstate(),
            Some("42883"),
            "{sql}"
        );
    }
}

#[test]
fn bound_numeric_functions_enforce_declared_integer_result_width() {
    for (ty, minimum) in [
        ("smallint", i64::from(i16::MIN)),
        ("integer", i64::from(i32::MIN)),
    ] {
        let scalar =
            crate::plan::ExpressionPlan::lower(expression(&format!("abs(({minimum})::{ty})")))
                .scalar;
        let bound = crate::bind_type_introspection(scalar, &RowSchema::default(), &[]);
        let crate::ScalarExpr::Func {
            binding: Some(binding),
            ..
        } = bound
        else {
            panic!("bound absolute value")
        };
        let context = EvalContext::new(None, &[]);
        assert_eq!(
            super::super::eval_bound_builtin_function_call(
                &binding,
                vec![(None, Value::Int(minimum))],
                &context
            )
            .unwrap_err()
            .sqlstate(),
            Some("22003"),
            "{ty}"
        );
        assert_eq!(
            super::super::eval_bound_builtin_function_call(
                &binding,
                vec![(None, Value::Int(minimum + 1))],
                &context
            )
            .unwrap(),
            Value::Int(-(minimum + 1)),
            "{ty}"
        );
    }
}

#[test]
fn operator_identity_survives_lowering_serialization_and_display_name_changes() {
    for (sql, operator) in [
        ("7 % 3", NumericOperator::Modulo),
        ("2 ^ 3", NumericOperator::Power),
        ("+ 1", NumericOperator::Plus),
        ("|/ 4", NumericOperator::SquareRoot),
        ("||/ 8", NumericOperator::CubeRoot),
        ("@ (-1)", NumericOperator::Absolute),
        ("7 OPERATOR(pg_catalog.%) 3", NumericOperator::Modulo),
        ("2 OPERATOR(pg_catalog.^) 3", NumericOperator::Power),
        ("OPERATOR(pg_catalog.+) 1", NumericOperator::Plus),
        ("OPERATOR(pg_catalog.|/) 4", NumericOperator::SquareRoot),
        ("OPERATOR(pg_catalog.||/) 8", NumericOperator::CubeRoot),
        ("OPERATOR(pg_catalog.@) (-1)", NumericOperator::Absolute),
    ] {
        let expr = expression(sql);
        let projection = crate::plan::ProjectionPlan {
            expr: crate::plan::ExpressionPlan::lower(expr.clone()).scalar,
            alias: None,
        };
        assert_eq!(
            crate::semantics::projection_label_at(&projection),
            "?column?",
            "{sql}"
        );
        for rendered in [
            crate::render::expression_sql(&expr).unwrap(),
            crate::catalog::expression_text::schema_expr_text(&expr),
        ] {
            assert_eq!(
                eval(&expression(&rendered), &EvalContext::new(None, &[])).unwrap(),
                eval(&expr, &EvalContext::new(None, &[])).unwrap()
            );
        }
        let serialized = serde_json::to_string(&expr).unwrap();
        let mut restored: Expr = serde_json::from_str(&serialized).unwrap();
        let Expr::Func {
            name,
            binding: Some(binding),
            ..
        } = &mut restored
        else {
            panic!("operator binding")
        };
        assert_eq!(
            binding.dispatch,
            Some(FunctionDispatch::NumericOperator(operator))
        );
        *name = "shadow.numeric".into();
        binding.name = "shadow.numeric".into();
        let scalar = crate::plan::ExpressionPlan::lower(restored.clone()).scalar;
        assert!(crate::scalar_type(&scalar, &RowSchema::default(), &[])
            .unwrap()
            .is_some());
        assert_eq!(
            eval(&restored, &EvalContext::new(None, &[])).unwrap(),
            eval(&expr, &EvalContext::new(None, &[])).unwrap()
        );
        let crate::ScalarExpr::Func {
            binding: Some(binding),
            ..
        } = scalar
        else {
            panic!("physical operator")
        };
        assert_eq!(
            binding.dispatch,
            Some(FunctionDispatch::NumericOperator(operator))
        );
    }
    for sql in ["mod(7,3)", "power(2,3)", "abs(-1)", "sqrt(4)", "cbrt(8)"] {
        let expr = expression(sql);
        let projection = crate::plan::ProjectionPlan {
            expr: crate::plan::ExpressionPlan::lower(expr.clone()).scalar,
            alias: None,
        };
        assert_eq!(
            crate::semantics::projection_label_at(&projection),
            sql.split('(').next().unwrap()
        );
        let Expr::Func { binding, .. } = expr else {
            panic!("ordinary function")
        };
        assert!(
            binding.and_then(|binding| binding.dispatch).is_none(),
            "{sql}"
        );
    }
}

struct OverrideFunctions;

impl crate::semantics::volatility::VolatilityCatalog for OverrideFunctions {
    fn host_function_volatility(&self, _: &str) -> Option<crate::ast::FunctionVolatility> {
        Some(crate::ast::FunctionVolatility::Volatile)
    }

    fn routine_volatilities(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
    ) -> Option<Vec<crate::ast::FunctionVolatility>> {
        Some(vec![crate::ast::FunctionVolatility::Volatile])
    }

    fn view_query(&self, _: &str) -> Result<Option<crate::plan::QueryPlan>> {
        Ok(None)
    }
}

impl EngineHook for OverrideFunctions {
    fn nextval(&self, _: &str) -> Result<i64> {
        unreachable!()
    }
    fn currval(&self, _: &str) -> Result<i64> {
        unreachable!()
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64> {
        unreachable!()
    }
    fn has_scalar_functions(&self) -> bool {
        true
    }
    fn call_scalar_function(&self, _: &str, _: &[Value]) -> Option<Result<Value>> {
        Some(Ok(Value::Int(99)))
    }
}

#[test]
fn runtime_functions_do_not_override_numeric_operator_syntax() {
    let hook = OverrideFunctions;
    let context = EvalContext::new(None, &[]).with_engine(&hook);
    assert_eq!(eval(&expression("7 % 3"), &context).unwrap(), Value::Int(1));
    let scalar = crate::plan::ExpressionPlan::lower(expression("7 % 3")).scalar;
    assert!(!crate::semantics::volatility::expr_contains_volatile_function(&hook, &scalar));
    let scalar = crate::plan::ExpressionPlan::lower(expression("7 % mod(3,2)")).scalar;
    assert!(crate::semantics::volatility::expr_contains_volatile_function(&hook, &scalar));
    assert_eq!(
        eval(&expression("mod(7,3)"), &context).unwrap(),
        Value::Int(99)
    );
}

struct OperandType(ColumnType);

impl crate::FunctionTypeResolver for OperandType {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>> {
        Ok(Some(self.0.clone()))
    }

    fn resolve_scalar_subquery_type(
        &self,
        _: crate::SubqueryId,
        _: &RowSchema,
        _: &[crate::SQLParam],
    ) -> Result<Option<ColumnType>> {
        Ok(Some(self.0.clone()))
    }
}

#[test]
fn bound_operator_retains_subquery_and_routine_types_without_a_runtime_resolver() {
    for sql in ["@ value_function()", "@ (SELECT 1)"] {
        let scalar = crate::plan::ExpressionPlan::lower(expression(sql)).scalar;
        let unbound = crate::bind_type_introspection(scalar.clone(), &RowSchema::default(), &[]);
        let crate::ScalarExpr::Func {
            binding: Some(binding),
            ..
        } = &unbound
        else {
            panic!("numeric operator")
        };
        assert!(binding.argument_types.is_empty());
        for (ty, state) in [
            (ColumnType::SmallInteger, "22003"),
            (ColumnType::Text, "42883"),
        ] {
            let bound = crate::bind_type_introspection_with_resolver(
                scalar.clone(),
                &RowSchema::default(),
                &[],
                &OperandType(ty.clone()),
            );
            if ty == ColumnType::SmallInteger {
                assert_eq!(
                    crate::scalar_type(&bound, &RowSchema::default(), &[]).unwrap(),
                    Some(ty)
                );
            } else {
                assert_eq!(
                    crate::scalar_type(&bound, &RowSchema::default(), &[])
                        .unwrap_err()
                        .sqlstate(),
                    Some(state)
                );
            }
            let crate::ScalarExpr::Func {
                binding: Some(binding),
                ..
            } = bound
            else {
                panic!("bound operator")
            };
            assert_eq!(
                super::super::eval_bound_builtin_function_call(
                    &binding,
                    vec![(None, Value::Int(-32768))],
                    &EvalContext::new(None, &[]),
                )
                .unwrap_err()
                .sqlstate(),
                Some(state)
            );
        }
    }
}
