//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute compiled statements with planning and statement scopes captured at their point of use.

use super::{context::StatementExecutionInputs, plan_executor::UnifiedPlanExecutor};
use uqa_sql::{
    binding::stored_routines::mark_catalog_statement_relations_bound,
    plan::{AggregateClassifier, CommandPlan, ExecutablePlanOptimizer, UnifiedPlan},
    SQLError, SQLParam, SQLResult, Statement,
};

pub struct CompiledStatementContext<'a, S: Clone + 'static> {
    pub aggregates: &'a dyn AggregateClassifier,
    pub planning: &'a dyn ExecutablePlanOptimizer,
    pub statements: &'a dyn StatementExecutionInputs<S>,
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
    let plan = context.planning.plan_for_execution(plan, params)?;
    UnifiedPlanExecutor::new_nested(context.statements.statement_execution_context(), params)
        .execute(&plan)
}

pub fn execute_with_privilege_subject<S: Clone + Send + Sync + 'static>(
    context: &CompiledStatementContext<'_, S>,
    statement: Statement,
    params: &[SQLParam],
    privilege_subject: &str,
) -> Result<SQLResult, SQLError> {
    let mut plan = UnifiedPlan::lower_with(statement, context.aggregates);
    mark_catalog_statement_relations_bound(&mut plan)?;
    let plan = context.planning.plan_for_execution(plan, params)?;
    UnifiedPlanExecutor::new_nested(context.statements.statement_execution_context(), params)
        .with_privilege_subject(privilege_subject)
        .execute(&plan)
}

pub fn execute_optimized_command<S: Clone + Send + Sync + 'static>(
    statements: &dyn StatementExecutionInputs<S>,
    command: &CommandPlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    UnifiedPlanExecutor::new_nested(statements.statement_execution_context(), params)
        .execute(&UnifiedPlan::Command(Box::new(command.clone())))
}

#[cfg(test)]
mod tests;
