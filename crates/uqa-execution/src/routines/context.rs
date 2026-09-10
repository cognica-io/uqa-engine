//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::transaction::RoutineSessionId;
use crate::query::runtime::QueryRuntimeView;
use uqa_core::Value;
use uqa_sql::{
    assignment::routines::RoutineValueContext,
    ast::{ColumnType, Expr, FetchCursorStmt, Statement},
    plan::{ExpressionPlan, UnifiedPlan},
    SQLError, SQLParam, SQLResult,
};

/// Bound expression evaluation in the routine's active namespace and parameter scope.
pub trait RoutineExpressions: RoutineValueContext {
    fn column_type_name(&self, name: &str) -> Result<ColumnType, SQLError>;
    fn evaluate(&self, expression: &Expr) -> Result<Value, SQLError>;
    fn evaluate_with_type(
        &self,
        expression: &Expr,
    ) -> Result<(Value, Option<ColumnType>), SQLError>;
    fn expression_type(&self, plan: &ExpressionPlan) -> Result<Option<ColumnType>, SQLError>;
}
/// Nested statement execution and planning retain the caller's active routine context.
pub trait RoutineStatements {
    fn execute_plan(&self, plan: &UnifiedPlan, params: &[SQLParam]) -> Result<SQLResult, SQLError>;
    fn execute_bound(
        &self,
        statement: Statement,
        params: &[SQLParam],
    ) -> Result<SQLResult, SQLError>;
    fn execute_text(&self, text: &str, params: &[SQLParam]) -> Result<SQLResult, SQLError>;
    fn optimize_plan(&self, plan: UnifiedPlan) -> Result<UnifiedPlan, SQLError>;
    fn assertions_enabled(&self) -> bool;
}
pub trait RoutineTransactions {
    fn depth(&self) -> usize;
    fn begin(&self) -> Result<(), SQLError>;
    fn commit(&self) -> Result<(), SQLError>;
    fn rollback(&self) -> Result<(), SQLError>;
    fn finish_procedural_transaction(&self, commit: bool, chain: bool) -> Result<(), SQLError>;
}
pub trait RoutinePortals {
    fn ensure_available(&self, name: &str) -> Result<(), SQLError>;
    fn open(
        &self,
        params: &[SQLParam],
        name: &str,
        scroll: Option<bool>,
        plan: &UnifiedPlan,
    ) -> Result<(), SQLError>;
    fn fetch(&self, request: &FetchCursorStmt) -> Result<SQLResult, SQLError>;
    fn close(&self, name: &str) -> Result<(), SQLError>;
    fn allocate_name(&self) -> String;
    fn pin(&self, name: &str) -> Result<(), SQLError>;
    fn unpin(&self, name: &str) -> Result<(), SQLError>;
}
#[derive(Clone, Copy)]
pub struct RoutineContext<'a> {
    pub expressions: &'a dyn RoutineExpressions,
    pub statements: &'a dyn RoutineStatements,
    pub transactions: &'a dyn RoutineTransactions,
    pub portals: &'a dyn RoutinePortals,
    pub session: RoutineSessionId,
    pub runtime: QueryRuntimeView<'a>,
}
