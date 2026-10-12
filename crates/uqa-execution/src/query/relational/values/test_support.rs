//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::query::{
    expression::ScalarExpressionContext,
    recheck_source::LockRowsRecheckSource,
    relational::{QueryExpressionFactory, RowLockOperatorFactory},
    routine_invocation::SQLRoutineInvoker,
    set_projection::SetFunctionRuntime,
    table_functions::{TableFunctionCall, TableFunctionRows},
};
use crate::scalar::plan::{PhysicalOuterRow, PhysicalSubqueryRunner, QueryExpressionContext};
use crate::{FunctionTypeResolver, PhysicalOperator, SharedRowPredicate, SubqueryResult};
use std::sync::Arc;
use uqa_core::Value;
use uqa_sql::{ast::FunctionBinding, expr::EngineHook, plan::QueryBlockPlan};

pub(crate) struct Services;

pub(crate) fn assert_values(sql: &str, actual: &[Vec<Value>]) {
    assert_eq!(actual.len(), 2, "{sql}");
    assert!(actual.iter().all(|row| row.len() == 1));
    if sql.contains("ARRAY[") {
        for (row, expected) in actual.iter().zip(["a", "b"]) {
            let array = row[0].array_view().unwrap();
            assert_eq!(array.elements(), &[Value::Str(expected.into())]);
            assert_eq!(array.lower_bounds(), &[1]);
        }
    } else if sql.starts_with("VALUES (NULL)") {
        assert_eq!(actual, &[vec![Value::Null], vec![Value::Str("a".into())]]);
    } else {
        assert_eq!(
            actual,
            &[vec![Value::Str("a".into())], vec![Value::Str("b".into())]]
        );
    }
}

impl FunctionTypeResolver for Services {
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
impl uqa_sql::routines::RoutineResolution for Services {}
impl uqa_sql::plan::AggregateClassifier for Services {
    fn is_registered_aggregate(&self, _: &str) -> bool {
        false
    }
}
impl crate::functions::AggregateFunctionRegistry for Services {
    fn registered_aggregate_function(
        &self,
        _: &str,
    ) -> Option<Arc<dyn crate::functions::SQLAggregateFunction>> {
        None
    }
}
impl EngineHook for Services {
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
impl PhysicalSubqueryRunner for Services {
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
impl QueryExpressionContext for Services {
    fn expression_evaluator<'a>(&'a self, _: &'a [SQLParam]) -> SharedExpressionEvaluator<'a> {
        panic!("unexpected nested evaluator")
    }
    fn subquery_plans(&self) -> &[QueryPlan] {
        &[]
    }
}
impl ScalarExpressionContext for Services {
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
impl SQLRoutineInvoker for Services {
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
}
impl SetFunctionRuntime for Services {
    fn has_registered_table_function(&self, _: &str) -> bool {
        false
    }
    fn table_function_rows(
        &self,
        _: TableFunctionCall<'_>,
        _: &[SQLParam],
        _: Option<&crate::OwnedPhysicalRow>,
    ) -> Result<TableFunctionRows, SQLError> {
        panic!("unexpected table function")
    }
}
impl QueryExpressionFactory<()> for Services {
    fn bind_scope(&self, scope: CteScope) -> Arc<dyn SetFunctionRuntime + '_> {
        assert!(scope.scalar_subqueries.is_empty());
        Arc::new(Self)
    }
    fn optimize_catalog_scalar(&self, _: &mut ScalarExpr) -> Result<(), SQLError> {
        panic!("unexpected optimization")
    }
    fn prepare_predicate<'a>(
        &'a self,
        _: &ScalarExpr,
        _: &'a [SQLParam],
        _: &CteScope,
    ) -> Result<Option<SharedRowPredicate<'a>>, SQLError> {
        panic!("unexpected predicate")
    }
}
impl RowLockOperatorFactory<()> for Services {
    fn attach_lock_rows<'a>(
        &'a self,
        _: Box<dyn PhysicalOperator + 'a>,
        _: &QueryBlockPlan,
        _: &'a [SQLParam],
        _: &CteScope,
        _: Option<u64>,
        _: Option<LockRowsRecheckSource<()>>,
    ) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
        panic!("unexpected row locking")
    }
}
impl crate::query::runtime::QueryMemorySettings for Services {
    fn work_mem_bytes(&self) -> Result<usize, SQLError> {
        Ok(1)
    }
}
