//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::scalar::plan::{PhysicalOuterRow, PhysicalSubqueryRunner};
use crate::{PhysicalRow, RowSchema, SubqueryResult};
use parking_lot::Mutex;
use uqa_sql::ast::{ColumnType, FunctionBinding, FunctionDispatch, NumericOperator};
use uqa_sql::expr::EngineHook;
use uqa_sql::plan::{AggregateClassifier, QueryPlan};
use uqa_sql::ResultRow;

#[derive(Default)]
struct Context {
    events: Mutex<Vec<&'static str>>,
    callback: bool,
}

impl EngineHook for Context {
    fn nextval(&self, _: &str) -> Result<i64, SQLError> {
        panic!("unexpected sequence call")
    }
    fn currval(&self, _: &str) -> Result<i64, SQLError> {
        panic!("unexpected sequence call")
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
        panic!("unexpected sequence call")
    }
    fn call_bound_user_function(
        &self,
        binding: &FunctionBinding,
        arguments: &[(Option<String>, Value)],
    ) -> Option<Result<Value, SQLError>> {
        assert_eq!(binding.object_id, Some([7; 16]));
        assert_eq!(arguments, &[(None, Value::Int(0))]);
        self.events.lock().push("user");
        Some(Ok(Value::Str("user".into())))
    }
    fn call_scalar_function(
        &self,
        name: &str,
        arguments: &[Value],
    ) -> Option<Result<Value, SQLError>> {
        if self.callback && name == "pg_get_viewdef" {
            assert_eq!(arguments, &[Value::Int(0)]);
            self.events.lock().push("callback");
            Some(Ok(Value::Str("callback".into())))
        } else {
            None
        }
    }
}

impl FunctionTypeResolver for Context {
    fn has_untyped_function(&self, name: &str) -> bool {
        self.callback && name == "pg_get_viewdef"
    }
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }
}

impl AggregateClassifier for Context {
    fn is_registered_aggregate(&self, _: &str) -> bool {
        false
    }
}

impl crate::functions::AggregateFunctionRegistry for Context {
    fn registered_aggregate_function(
        &self,
        _: &str,
    ) -> Option<Arc<dyn crate::functions::SQLAggregateFunction>> {
        None
    }
}

impl PhysicalSubqueryRunner for Context {
    fn execute_subquery(
        &self,
        _: usize,
        _: &QueryPlan,
        _: PhysicalOuterRow<'_>,
        _: &[SQLParam],
    ) -> Result<SubqueryResult, SQLError> {
        panic!("unexpected subquery")
    }
}

impl QueryExpressionContext for Context {
    fn expression_evaluator<'a>(&'a self, _: &'a [SQLParam]) -> SharedExpressionEvaluator<'a> {
        panic!("unexpected nested evaluator")
    }
    fn subquery_plans(&self) -> &[QueryPlan] {
        &[]
    }
}

impl ScalarExpressionContext for Context {
    fn intercept_function(
        &self,
        name: &str,
        arguments: &[ScalarExpr],
        _: &dyn RowLookup,
        evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
    ) -> Result<Option<Value>, SQLError> {
        if matches!(name, "pg_get_viewdef" | "pg_catalog.pg_get_viewdef") {
            self.events.lock().push("catalog");
            for argument in arguments {
                evaluate(argument)?;
            }
            Ok(Some(Value::Str("catalog".into())))
        } else {
            Ok(None)
        }
    }
}

fn binding(builtin: bool) -> FunctionBinding {
    FunctionBinding {
        object_id: (!builtin).then_some([7; 16]),
        name: if builtin {
            "pg_catalog.pg_get_viewdef"
        } else {
            "shadow.pg_get_viewdef"
        }
        .into(),
        argument_types: vec!["oid".into()],
        builtin,
        dispatch: None,
        invocation: None,
        resolution_error: None,
    }
}

fn call(name: &str, binding: Option<FunctionBinding>) -> ScalarExpr {
    ScalarExpr::Func {
        name: name.into(),
        binding,
        args: vec![ScalarExpr::Literal(Value::Int(0))],
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    }
}

fn evaluate_both(context: &Arc<Context>, expression: &ScalarExpr) -> [ExecResult<Value>; 2] {
    let evaluator =
        ScopedExpressionEvaluator::shared(context.clone(), &[], uqa_core::CancellationToken::new());
    [
        evaluator.evaluate(expression, &ResultRow::new()),
        evaluator.evaluate_physical(
            expression,
            &RowSchema::default(),
            &PhysicalRow::from_values(Vec::new()),
        ),
    ]
}

#[test]
fn selected_user_routines_and_callbacks_keep_dispatch_precedence() {
    let context = Arc::new(Context::default());
    for result in evaluate_both(&context, &call("pg_get_viewdef", Some(binding(false)))) {
        assert_eq!(result.unwrap(), Value::Str("user".into()));
    }
    assert_eq!(*context.events.lock(), ["user", "user"]);
    let context = Arc::new(Context {
        callback: true,
        ..Context::default()
    });
    for result in evaluate_both(&context, &call("pg_get_viewdef", None)) {
        assert_eq!(result.unwrap(), Value::Str("callback".into()));
    }
    assert_eq!(*context.events.lock(), ["callback", "callback"]);
}

#[test]
fn catalog_interception_uses_the_bound_identity() {
    let context = Arc::new(Context {
        callback: true,
        ..Context::default()
    });
    for name in ["pg_get_viewdef", "old_display_name"] {
        for result in evaluate_both(&context, &call(name, Some(binding(true)))) {
            assert_eq!(result.unwrap(), Value::Str("catalog".into()));
        }
    }
    assert_eq!(*context.events.lock(), ["catalog"; 4]);
    let context = Arc::new(Context::default());
    for result in evaluate_both(&context, &call("pg_get_viewdef", None)) {
        assert_eq!(result.unwrap(), Value::Str("catalog".into()));
    }
    assert_eq!(*context.events.lock(), ["catalog"; 2]);
}

#[test]
fn binding_errors_and_structural_operators_bypass_name_interception() {
    let context = Arc::new(Context::default());
    let expression = call(
        "pg_get_viewdef",
        Some(FunctionBinding::undefined_function(
            "pg_get_viewdef",
            "pg_get_viewdef(boolean)",
        )),
    );
    for result in evaluate_both(&context, &expression) {
        let crate::ExecError::SQL(error) = result.unwrap_err() else {
            panic!("function binding lost its SQL error");
        };
        assert_eq!(error.sqlstate(), Some("42883"));
    }
    let expression = call(
        "pg_get_viewdef",
        Some(FunctionBinding::dispatched(
            FunctionDispatch::NumericOperator(NumericOperator::Absolute),
        )),
    );
    for result in evaluate_both(&context, &expression) {
        assert_eq!(result.unwrap(), Value::Int(0));
    }
    assert!(context.events.lock().is_empty());
}
