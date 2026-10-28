//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute compiled statements with planning and statement scopes captured at their point of use.

use super::{context::StatementExecutionInputs, plan_executor::UnifiedPlanExecutor};
use uqa_sql::catalog::roles::RoleReference;
use uqa_sql::{
    binding::statements::AnalyzedResult,
    binding::stored_routines::mark_catalog_statement_relations_bound,
    plan::{AggregateClassifier, CommandPlan, ExecutablePlanOptimizer, UnifiedPlan},
    SQLError, SQLParam, SQLResult, Statement,
};

pub struct CompiledStatementContext<'a, S: Clone + 'static> {
    pub aggregates: &'a dyn AggregateClassifier,
    pub planning: &'a dyn ExecutablePlanOptimizer,
    pub statements: &'a dyn StatementExecutionInputs<S>,
    /// The session's cancellation, which the statement's analysis, planning and execution observe.
    pub cancellation: &'a uqa_core::CancellationToken,
}

pub fn execute<S: Clone + Send + Sync + 'static>(
    context: &CompiledStatementContext<'_, S>,
    statement: Statement,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    let plan = UnifiedPlan::lower_with(statement, context.aggregates);
    execute_plan(context, plan, params)
}

pub fn execute_plan<S: Clone + Send + Sync + 'static>(
    context: &CompiledStatementContext<'_, S>,
    plan: UnifiedPlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    let _statement_thread =
        super::context::statement_thread_scope(context.statements, context.cancellation);
    let plan = context.planning.plan_for_execution(plan, params)?;
    UnifiedPlanExecutor::new_nested(context.statements.statement_execution_context(), params)
        .execute(&plan)
}

/// A prepared owner has already analyzed and selected this executable. Capture
/// fresh execution inputs without binding or optimizing it a second time.
pub fn execute_selected_plan<S: Clone + Send + Sync + 'static>(
    context: &CompiledStatementContext<'_, S>,
    plan: &UnifiedPlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    let _statement_thread =
        super::context::statement_thread_scope(context.statements, context.cancellation);
    UnifiedPlanExecutor::new_nested(context.statements.statement_execution_context(), params)
        .execute(plan)
}

/// Run a statement once `check` accepts what its analysis derives about its result.
pub fn execute_checked_plan<S: Clone + Send + Sync + 'static>(
    context: &CompiledStatementContext<'_, S>,
    plan: UnifiedPlan,
    params: &[SQLParam],
    check: &dyn Fn(&AnalyzedResult) -> Result<(), SQLError>,
) -> Result<SQLResult, SQLError> {
    let _statement_thread =
        super::context::statement_thread_scope(context.statements, context.cancellation);
    let (plan, result) = context.planning.plan_with_result(plan, params)?;
    check(&result)?;
    UnifiedPlanExecutor::new_nested(context.statements.statement_execution_context(), params)
        .execute(&plan)
}

pub fn execute_with_privilege_subject<S: Clone + Send + Sync + 'static>(
    context: &CompiledStatementContext<'_, S>,
    statement: Statement,
    params: &[SQLParam],
    privilege_subject: &RoleReference,
) -> Result<SQLResult, SQLError> {
    let _statement_thread =
        super::context::statement_thread_scope(context.statements, context.cancellation);
    let plan = stored_plan(context, statement, params)?;
    UnifiedPlanExecutor::new_nested(context.statements.statement_execution_context(), params)
        .with_privilege_subject(privilege_subject)
        .execute(&plan)
}

/// Finish a stored rule action under its owner, keeping query and mutation output in bounded physical storage.
pub fn execute_output_with_privilege_subject<S: Clone + Send + Sync + 'static>(
    context: &CompiledStatementContext<'_, S>,
    action: uqa_sql::semantics::rules::binding::RuleAction,
    params: &[SQLParam],
    inherited_ctes: Option<&crate::query::CteScope<S>>,
    privilege_subject: &RoleReference,
    consumer: Option<std::rc::Rc<dyn crate::query::consumer::QueryRowConsumer + '_>>,
) -> Result<super::plan_executor::StatementOutput, SQLError> {
    let _statement_thread =
        super::context::statement_thread_scope(context.statements, context.cancellation);
    let plan = action.lower(context.aggregates)?;
    let plan = context.planning.plan_for_execution(plan, params)?;
    UnifiedPlanExecutor::new_nested(context.statements.statement_execution_context(), params)
        .with_privilege_subject(privilege_subject)
        .with_inherited_ctes(inherited_ctes)
        .execute_for_output_mode(
            &plan,
            consumer.map_or(
                crate::query::statement::consumer::QueryOutputMode::SharedSpill,
                crate::query::statement::consumer::QueryOutputMode::physical_consumer,
            ),
        )
}

fn stored_plan<S: Clone + 'static>(
    context: &CompiledStatementContext<'_, S>,
    statement: Statement,
    params: &[SQLParam],
) -> Result<uqa_sql::plan::SharedExecutablePlan, SQLError> {
    let mut plan = UnifiedPlan::lower_with(statement, context.aggregates);
    mark_catalog_statement_relations_bound(&mut plan)?;
    context.planning.plan_for_execution(plan, params)
}

pub fn execute_optimized_command<S: Clone + Send + Sync + 'static>(
    context: &CompiledStatementContext<'_, S>,
    command: &CommandPlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    let _statement_thread =
        super::context::statement_thread_scope(context.statements, context.cancellation);
    UnifiedPlanExecutor::new_nested(context.statements.statement_execution_context(), params)
        .execute(&UnifiedPlan::Command(Box::new(command.clone())))
}

#[cfg(test)]
mod tests;
