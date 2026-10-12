//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::atomic::{AtomicI64, Ordering};

#[derive(Default)]
struct Sequence(AtomicI64);

impl EngineHook for Sequence {
    fn nextval(&self, _: &str) -> Result<i64> {
        Ok(self.0.fetch_add(1, Ordering::Relaxed) + 1)
    }
    fn currval(&self, _: &str) -> Result<i64> {
        Ok(self.0.load(Ordering::Relaxed))
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64> {
        unreachable!("reference never sets the sequence")
    }
    fn require_builtin_execute(&self, binding: &FunctionBinding) -> Result<()> {
        assert!(
            !matches!(binding.dispatch, Some(FunctionDispatch::Collate { .. })),
            "a collation clause is not a function call"
        );
        Ok(())
    }

    fn call_scalar_function(&self, name: &str, _: &[Value]) -> Option<Result<Value>> {
        (name == "\"collate\"").then(|| Ok(Value::Str("routine".into())))
    }
}

#[test]
fn collation_execution_keeps_routine_names_and_retained_errors_distinct() {
    let hook = Sequence::default();
    let context = EvalContext::new(None, &[]).with_engine(&hook);
    let crate::Statement::Select(query) =
        crate::compile("SELECT \"COLLATE\"('x')").unwrap().remove(0)
    else {
        panic!("SELECT")
    };
    assert_eq!(
        eval(&query.projections[0].expr, &context).unwrap(),
        Value::Str("routine".into())
    );
    let crate::Statement::Select(mut query) =
        crate::compile("SELECT nextval('effect')::text COLLATE \"C\"")
            .unwrap()
            .remove(0)
    else {
        panic!("SELECT")
    };
    let expression = &mut query.projections[0].expr;
    let Expr::Func {
        binding: Some(binding),
        ..
    } = expression
    else {
        panic!("clause")
    };
    binding.resolution_error = Some(Box::new(crate::ast::FunctionResolutionError::Operator(
        Box::new(crate::ast::OperatorResolutionError {
            sqlstate: "42804".into(),
            message: "retained type error".into(),
        }),
    )));
    let error = eval(expression, &context).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42804"));
    assert_eq!(error.to_string(), "retained type error");
    assert_eq!(hook.0.load(Ordering::Relaxed), 0);
}

#[test]
fn collation_value_evaluation_matches_postgresql_without_function_dispatch() {
    let cases: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("collation_values.json")).unwrap();
    let hook = Sequence::default();
    let context = EvalContext::new(None, &[]).with_engine(&hook);
    let mut failures = Vec::new();
    for case in cases {
        let source = case["expression"].as_str().unwrap();
        let crate::Statement::Select(mut query) = crate::compile(&format!("SELECT {source}"))
            .unwrap()
            .remove(0)
        else {
            panic!("SELECT")
        };
        let expression = query.projections.remove(0).expr;
        let scalar = crate::plan::ExpressionPlan::lower(expression.clone()).scalar;
        let ty = crate::scalar_type(&scalar, &crate::RowSchema::default(), &[])
            .unwrap()
            .unwrap();
        let observed = eval(&expression, &context);
        if case["error"].is_null() {
            if crate::catalog::type_metadata::pg_type_oid(&ty) != case["type_oid"].as_i64().unwrap()
            {
                failures.push(format!("{source}: type {ty:?}"));
            }
            let output = observed.and_then(|value| {
                if value == Value::Null {
                    Ok(None)
                } else {
                    value_to_string(&value).map(Some)
                }
            });
            if output.as_ref().ok().map(|value| value.as_deref()) != Some(case["value"].as_str()) {
                failures.push(format!("{source}: {output:?}"));
            }
        } else {
            match observed {
                Err(error)
                    if error.sqlstate() == case["error"]["sqlstate"].as_str()
                        && error.to_string() == case["error"]["message"].as_str().unwrap() => {}
                result => failures.push(format!("{source}: {result:?}")),
            }
        }
    }
    assert_eq!(
        hook.0.load(Ordering::Relaxed),
        1,
        "operand evaluated exactly once"
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
