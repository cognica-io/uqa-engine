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
    fn expression_type_with_control(
        &self,
        plan: &ExpressionPlan,
        params: &[SQLParam],
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<ColumnType>>, SQLError> {
        control.check()?;
        self.expression_type(plan, params)?
            .as_ref()
            .map(|ty| ty.clone_with_control(control).map_err(Into::into))
            .transpose()
    }
    fn column_type_name(&self, name: &str) -> Result<ColumnType, SQLError>;
    fn evaluate(&self, expression: &Expr) -> Result<Value, SQLError>;
    fn evaluate_with_type(
        &self,
        expression: &Expr,
    ) -> Result<crate::query::catalog_expression::TypedCatalogValue, SQLError>;
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
    /// Retain the native request resources for statement preparation inside an activation. External executors without an admission contract preserve their ordinary allocation behavior.
    fn planning_resources(&self) -> Result<uqa_core::memory::OwnedProductionControl, SQLError> {
        Ok(uqa_core::memory::ProductionControl::uncontrolled().capture())
    }

    fn body_input_context(&self) -> Option<super::sql_body::inputs::SQLRoutineInputContext<'_>> {
        None
    }

    fn plpgsql_preparations(
        &self,
        definition: &uqa_sql::ast::CreateFunction,
        parsed: &uqa_sql::plpgsql::PLpgSQLFunction,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<std::sync::Arc<super::preparation::PLpgSQLPreparations>, SQLError>;
    /// SQL analysis only: publication precedes optimization and statement effects.
    fn analyze_static_plan(
        &self,
        plan: &mut UnifiedPlan,
        parameters: &[SQLParam],
    ) -> Result<uqa_sql::binding::statements::ProceduralPlanAnalysis, SQLError>;
    fn analyze_static_plan_owned(
        &self,
        plan: uqa_sql::plan::MutableExecutablePlan,
        parameters: &[SQLParam],
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_sql::binding::statements::OwnedProceduralPlanAnalysis, SQLError> {
        if control.budget().is_some() {
            return Err(SQLError::Unsupported(
                "this routine executor has no owning analysis producer".into(),
            ));
        }
        let mut plan = plan.finish(control)?.into_parts().0;
        let analysis = self.analyze_static_plan(&mut plan, parameters)?;
        let definition = analysis.prepared_definition(&plan, parameters)?;
        Ok(uqa_sql::binding::statements::OwnedProceduralPlanAnalysis {
            plan: definition.logical_plan.into(),
            composite_inputs: definition.composite_inputs,
            result: analysis.result,
            parameter_types: definition.parameter_types,
            dependencies: definition.dependencies,
            effective_search_path: definition.effective_search_path,
            dependency_snapshot: definition.dependency_snapshot,
        })
    }
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
    /// Reuse the ordinary custom/generic selection policy for a retained routine
    /// statement, without registering it in the user's PREPARE namespace.
    fn select_body_plan(
        &self,
        entry: &uqa_sql::prepared::entry::PreparedStatementPlan,
        params: &[SQLParam],
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_sql::prepared::planning::PreparedPlanSelection, SQLError>;
    /// Select the generic plan of an analyzed PL/pgSQL simple expression, preserving runtime parameters.
    fn select_simple_expression_plan(
        &self,
        entry: &uqa_sql::prepared::entry::PreparedStatementPlan,
        params: &[SQLParam],
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_sql::prepared::planning::PreparedPlanSelection, SQLError>;
    /// Execute an already selected plan under the current statement scope.
    fn execute_selected_body_plan(
        &self,
        plan: &UnifiedPlan,
        params: &[SQLParam],
    ) -> Result<SQLResult, SQLError>;
    fn execute_bound(
        &self,
        statement: Statement,
        params: &[SQLParam],
    ) -> Result<SQLResult, SQLError>;
    fn execute_text(&self, text: &str, params: &[SQLParam]) -> Result<SQLResult, SQLError>;
    fn optimize_plan(
        &self,
        plan: UnifiedPlan,
    ) -> Result<uqa_sql::plan::SharedExecutablePlan, SQLError>;
    fn optimize_owned_plan(
        &self,
        plan: uqa_sql::plan::MutableExecutablePlan,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_sql::plan::SharedExecutablePlan, SQLError> {
        control.check()?;
        if control.budget().is_some() {
            return Err(SQLError::Unsupported(
                "routine adapter has no owned plan optimizer".into(),
            ));
        }
        self.optimize_plan(plan.finish(control)?.into_parts().0)
    }
    fn assertions_enabled(&self) -> bool;
    /// Load the library of a procedural language into the session, as its call handler does on first use.
    fn load_language_library(&self, language: &str);
    /// Run `analyze` with the routines and the catalog, namespace and transition relations that a statement the routine runs now is analyzed with.
    fn with_statement_scope(&self, analyze: StatementScopeOperation<'_>) -> Result<(), SQLError>;
    fn with_statement_scope_with_control(
        &self,
        control: &uqa_core::memory::ProductionControl<'_>,
        analyze: ControlledStatementScopeOperation<'_>,
    ) -> Result<(), SQLError> {
        self.with_statement_scope(&mut |routines, binding| {
            analyze(routines, binding.copy_with_control(control)?)
        })
    }
}
/// An analysis that runs with the routines and the binding scope of a statement the routine runs.
pub type StatementScopeOperation<'a> =
    &'a mut dyn FnMut(&dyn RoutineResolution, &BindingContext<'_>) -> Result<(), SQLError>;
pub type ControlledStatementScopeOperation<'a> = &'a mut dyn FnMut(
    &dyn RoutineResolution,
    uqa_sql::binding::production::ControlledBindingContext<'_>,
) -> Result<(), SQLError>;
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
    /// A nested planning invocation keeps the exact caller allowance and both cancellation scopes through its routine's internal statement preparation.
    pub planning: Option<uqa_core::memory::ProductionControl<'a>>,
}

impl RoutineContext<'_> {
    pub fn planning_resources(&self) -> Result<uqa_core::memory::OwnedProductionControl, SQLError> {
        self.planning.map_or_else(
            || self.statements.planning_resources(),
            |control| Ok(control.capture()),
        )
    }
}
