//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve DML targets and enter one mutation command with its inherited CTE scope.
use super::{dispatch, insert::table::InsertPlanning};
use crate::query::CteScope;
use uqa_sql::{
    plan::{CommandPlan, DeletePlan, InsertPlan, MergePlan, UpdatePlan},
    SQLError, SQLParam, SQLResult,
};
mod context;
pub use context::*;

pub fn run_insert<S: Clone + Send + Sync + 'static>(
    context: &MutationEntryContext<'_, S>,
    mut statement: InsertPlan,
    params: &[SQLParam],
    ctes: Option<&CteScope<S>>,
) -> Result<SQLResult, SQLError> {
    statement.table = context
        .targets
        .resolve_target(&statement.table, statement.target_relation_bound)?;
    context.transactions.with_command(Box::new(move |context| {
        dispatch::run_insert(
            &context.statement,
            InsertPlanning {
                inference: context.inference,
                returning: context.returning,
                prune_source_outputs: context.prune_source_outputs,
            },
            &statement,
            params,
            ctes,
        )
    }))
}

pub fn run_update<S: Clone + Send + Sync + 'static>(
    context: &MutationEntryContext<'_, S>,
    mut statement: UpdatePlan,
    params: &[SQLParam],
    ctes: Option<&CteScope<S>>,
) -> Result<SQLResult, SQLError> {
    statement.table = context
        .targets
        .resolve_target(&statement.table, statement.target_relation_bound)?;
    context.transactions.with_command(Box::new(move |context| {
        dispatch::run_update(
            &context.statement,
            context.prune_source_outputs,
            &statement,
            params,
            ctes,
        )
    }))
}

pub fn run_delete<S: Clone + Send + Sync + 'static>(
    context: &MutationEntryContext<'_, S>,
    mut statement: DeletePlan,
    params: &[SQLParam],
    ctes: Option<&CteScope<S>>,
) -> Result<SQLResult, SQLError> {
    statement.table = context
        .targets
        .resolve_target(&statement.table, statement.target_relation_bound)?;
    if ctes.is_none() {
        uqa_sql::semantics::returning::validate_returning_alias_relations(
            &statement.target_qualifier,
            &statement.returning_aliases,
            None,
        )?;
    }
    context.transactions.with_command(Box::new(move |context| {
        dispatch::run_delete(
            &context.statement,
            context.prune_source_outputs,
            &statement,
            params,
            ctes,
        )
    }))
}

pub fn run_merge<S: Clone + Send + Sync + 'static>(
    context: &MutationEntryContext<'_, S>,
    mut statement: MergePlan,
    params: &[SQLParam],
    ctes: Option<&CteScope<S>>,
) -> Result<SQLResult, SQLError> {
    statement.target = context.targets.resolve_target(&statement.target, false)?;
    context.transactions.with_command(Box::new(move |context| {
        dispatch::run_merge(
            &context.statement,
            context.prune_source_outputs,
            &statement,
            params,
            ctes,
        )
    }))
}

pub fn execute_cte_command<S: Clone + Send + Sync + 'static>(
    context: &MutationEntryContext<'_, S>,
    command: &CommandPlan,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<SQLResult, SQLError> {
    if let Some(error) = uqa_sql::semantics::virtual_relation_mutation_error(
        &ctes.relation_name_resolution()?,
        command,
    ) {
        crate::query::binding::analyze_command_parameters(context.routines, command, params, ctes)?;
        return Err(error);
    }
    let mut command = command.clone();
    let subject = ctes.privilege_subject()?.to_string();
    uqa_sql::semantics::mutation_privileges::inherit_command_privilege_subject(
        &mut command,
        subject,
    )?;
    match command {
        CommandPlan::Insert(plan) => run_insert(context, *plan, params, Some(ctes)),
        CommandPlan::Update(plan) => run_update(context, *plan, params, Some(ctes)),
        CommandPlan::Delete(plan) => run_delete(context, *plan, params, Some(ctes)),
        CommandPlan::Merge(plan) => run_merge(context, *plan, params, Some(ctes)),
        _ => Err(SQLError::Internal(
            "non-DML command in a WITH definition".into(),
        )),
    }
}

pub fn cursor_command_returning_schema<S: Clone + 'static>(
    execution: &super::returning::ReturningExecutionContext<'_, S>,
    analysis: uqa_sql::semantics::returning::ReturningAnalysisContext<'_>,
    scopes: &dyn super::command_scope::CommandScopeSource<S>,
    command: &CommandPlan,
    params: &[SQLParam],
) -> Result<Option<crate::RowSchema>, SQLError> {
    match command {
        CommandPlan::Merge(plan) => {
            super::merge::analysis::merge_command_returning_schema(execution, scopes, plan, params)
        }
        _ => uqa_sql::semantics::returning::dml_command_returning_schema(analysis, command, params),
    }
}
