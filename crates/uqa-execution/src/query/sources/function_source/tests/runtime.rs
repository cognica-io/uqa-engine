//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::query::{
    expression::ScalarExpressionContext,
    routine_invocation::SQLRoutineInvoker,
    set_projection::SetFunctionRuntime,
    table_functions::{TableFunctionCall, TableFunctionRows},
};
use crate::scalar::plan::{PhysicalOuterRow, PhysicalSubqueryRunner, QueryExpressionContext};
use crate::{FunctionTypeResolver, ScalarExpr, SharedExpressionEvaluator, SubqueryResult};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use uqa_core::Value;
use uqa_sql::{
    ast::FunctionBinding, expr::EngineHook, plan::QueryPlan, ColumnType, SQLError, SQLParam,
};

#[derive(Default)]
pub(super) struct Runtime {
    pub(super) calls: AtomicUsize,
    pub(super) scope:
        Mutex<Option<uqa_sql::routines::invocation::descriptions::RoutineDescriptionScope>>,
}

impl FunctionTypeResolver for Runtime {
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
impl uqa_sql::routines::RoutineResolution for Runtime {}
impl uqa_sql::plan::AggregateClassifier for Runtime {
    fn is_registered_aggregate(&self, _: &str) -> bool {
        false
    }
}
impl crate::functions::AggregateFunctionRegistry for Runtime {
    fn registered_aggregate_function(
        &self,
        _: &str,
    ) -> Option<Arc<dyn crate::functions::SQLAggregateFactory>> {
        None
    }
}
impl EngineHook for Runtime {
    fn routine_description_scope(
        &self,
    ) -> Option<uqa_sql::routines::invocation::descriptions::RoutineDescriptionScope> {
        self.scope.lock().unwrap().clone()
    }
    fn nextval(&self, _: &str) -> Result<i64, SQLError> {
        panic!("unexpected sequence access")
    }
    fn currval(&self, _: &str) -> Result<i64, SQLError> {
        panic!("unexpected sequence access")
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
        panic!("unexpected sequence access")
    }
}
impl PhysicalSubqueryRunner for Runtime {
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
impl QueryExpressionContext for Runtime {
    fn expression_evaluator<'a>(
        &'a self,
        _: &'a [SQLParam],
    ) -> Result<SharedExpressionEvaluator<'a>, SQLError> {
        panic!("unexpected nested evaluator")
    }
    fn subquery_plans(&self) -> &[QueryPlan] {
        &[]
    }
}
impl ScalarExpressionContext for Runtime {
    fn intercept_function(
        &self,
        _: &str,
        _: &[ScalarExpr],
        _: &dyn uqa_sql::expr::RowLookup,
        _: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
    ) -> Result<Option<Value>, SQLError> {
        Ok(None)
    }
}
impl SQLRoutineInvoker for Runtime {
    fn has_user_function(&self, _: &str, _: Option<&FunctionBinding>) -> Result<bool, SQLError> {
        Ok(false)
    }
    fn user_function_returns_set(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
        _: &[(Option<String>, Value)],
    ) -> Option<Result<bool, SQLError>> {
        None
    }
    fn call_user_scalar_function(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
        _: &[(Option<String>, Value)],
    ) -> Option<Result<Value, SQLError>> {
        panic!("unexpected user routine")
    }
    fn call_user_table_function(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
        _: &[(Option<String>, Value)],
    ) -> Option<Result<crate::functions::SQLTableFunctionResult, SQLError>> {
        panic!("unexpected user routine")
    }
    fn require_user_function_execute(
        &self,
        _: &uqa_sql::routines::SQLUserFunction,
    ) -> Result<(), SQLError> {
        panic!("unexpected user routine")
    }
}
impl SetFunctionRuntime for Runtime {
    fn has_registered_table_function(&self, _: &str) -> bool {
        true
    }
    fn table_function_rows(
        &self,
        call: TableFunctionCall<'_>,
        _: &[SQLParam],
        row: Option<&crate::OwnedPhysicalRow>,
    ) -> Result<TableFunctionRows, SQLError> {
        let ScalarExpr::Literal(Value::Str(payload)) = &call.args[0] else {
            panic!("payload argument")
        };
        assert!(payload.bytes().all(|byte| byte == b'x'));
        let input = row.unwrap().physical_value_at(0).unwrap().clone();
        self.calls.fetch_add(1, Ordering::Relaxed);
        let mut rows = vec![vec![input.clone(), Value::Int(payload.len() as i64)]];
        if call.name == "two_payload" {
            let Value::Int(input) = input else {
                panic!("integer input")
            };
            rows.push(vec![
                Value::Int(input + 1),
                Value::Int(payload.len() as i64),
            ]);
        }
        Ok(TableFunctionRows::materialized(
            vec!["input".into(), "payload_length".into()],
            rows,
        ))
    }
}
