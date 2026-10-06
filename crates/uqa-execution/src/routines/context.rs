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
    binding::{statements::AnalyzedResult, BindingContext},
    plan::{ExpressionPlan, UnifiedPlan},
    routines::RoutineResolution,
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
    fn expression_type(
        &self,
        plan: &ExpressionPlan,
        params: &[SQLParam],
    ) -> Result<Option<ColumnType>, SQLError>;
}
/// A check of what a statement's analysis derives about its result, made before the statement runs.
pub type StatementResultCheck<'a> = &'a dyn Fn(&AnalyzedResult) -> Result<(), SQLError>;
/// Nested statement execution and planning retain the caller's active routine context.
pub trait RoutineStatements {
    fn body_input_context(&self) -> Option<super::sql_body::inputs::SQLRoutineInputContext<'_>> {
        None
    }

    fn plpgsql_preparations(
        &self,
        definition: &uqa_sql::ast::CreateFunction,
        parsed: &uqa_sql::plpgsql::PLpgSQLFunction,
    ) -> std::sync::Arc<super::preparation::PLpgSQLPreparations>;
    /// SQL analysis only: publication precedes optimization and statement effects.
    fn analyze_static_plan(
        &self,
        plan: &mut UnifiedPlan,
        parameters: &[SQLParam],
    ) -> Result<uqa_sql::binding::statements::ProceduralPlanAnalysis, SQLError>;
    fn parser_settings(&self) -> uqa_sql::parser::ParserSettings {
        uqa_sql::parser::ParserSettings::default()
    }
    /// Run one statement of a SQL function body; `check` sees what the statement's analysis derives about its result before it runs, as `PostgreSQL` checks a body's final statement before running it.
    fn execute_body_statement(
        &self,
        plan: UnifiedPlan,
        params: &[SQLParam],
        check: Option<StatementResultCheck<'_>>,
    ) -> Result<SQLResult, SQLError>;
    fn execute_bound(
        &self,
        statement: Statement,
        params: &[SQLParam],
    ) -> Result<SQLResult, SQLError>;
    fn execute_text(&self, text: &str, params: &[SQLParam]) -> Result<SQLResult, SQLError>;
    fn optimize_plan(&self, plan: UnifiedPlan) -> Result<UnifiedPlan, SQLError>;
    fn assertions_enabled(&self) -> bool;
    /// Load the library of a procedural language into the session, as its call handler does on first use.
    fn load_language_library(&self, language: &str);
    /// Run `analyze` with the routines and the catalog, namespace and transition relations that a statement the routine runs now is analyzed with.
    fn with_statement_scope(&self, analyze: StatementScopeOperation<'_>) -> Result<(), SQLError>;
}
/// An analysis that runs with the routines and the binding scope of a statement the routine runs.
pub type StatementScopeOperation<'a> =
    &'a mut dyn FnMut(&dyn RoutineResolution, &BindingContext<'_>) -> Result<(), SQLError>;
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
        source_sql: &str,
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
