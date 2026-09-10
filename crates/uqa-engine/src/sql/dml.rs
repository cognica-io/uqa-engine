//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL DML execution, constraints, referential actions, and RETURNING rows.

use super::{CteScope, Engine, SQLError, SQLParam, SQLResult};
use uqa_planner::{DeletePlan, InsertPlan, MergePlan, UpdatePlan};

mod protocol;

pub(in crate::sql) use protocol::*;
pub(crate) use protocol::{
    CommandExactIndex, CommandMutationOverlay, CommandStoredDocument, DeferredForeignKeyCheck,
    TransactionRowChange,
};

pub(in crate::sql) use uqa_planner::mutation_outputs::prune_unused_query_outputs;

/// Resolve a statement's mutation target once, before any internal storage or rewrite path can observe its textual name.
pub(in crate::sql) fn resolve_dml_target_name(
    engine: &Engine,
    name: &str,
    target_relation_bound: bool,
) -> Result<String, SQLError> {
    engine.resolve_mutation_target_name(name, target_relation_bound)
}

pub(crate) fn update_lock_strength(
    engine: &Engine,
    table: &str,
    columns: &[String],
) -> uqa_sql::ast::LockStrength {
    uqa_execution::query::locking::context::update_lock_strength(engine, table, columns)
}

mod conflict;
mod constraints;
mod delete;
mod insert;
mod merge;
mod update;
pub(in crate::sql) mod view_automatic;

pub(in crate::sql) use conflict::*;
pub(crate) use constraints::*;
pub(in crate::sql) use delete::*;
pub(in crate::sql) use insert::*;
pub(in crate::sql) use merge::*;
pub(in crate::sql) use update::*;

pub(in crate::sql) fn execute_cte_command(
    engine: &Engine,
    command: &uqa_planner::CommandPlan,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<SQLResult, SQLError> {
    if let Some(error) =
        super::catalog::virtual_relation_mutation_error(&ctes.relation_name_resolution()?, command)
    {
        super::prepared::analyze_command_parameters(engine, command, params, ctes)?;
        return Err(error);
    }
    let mut command = command.clone();
    let subject = ctes.privilege_subject()?.to_string();
    match &mut command {
        uqa_planner::CommandPlan::Insert(plan) => {
            plan.statement_privilege_subject
                .get_or_insert(subject.clone());
            plan.target_privilege_subject.get_or_insert(subject);
            insert::run_insert_with_ctes(engine, *plan.clone(), params, ctes)
        }
        uqa_planner::CommandPlan::Update(plan) => {
            plan.statement_privilege_subject
                .get_or_insert(subject.clone());
            plan.target_privilege_subject.get_or_insert(subject);
            update::run_update_with_ctes(engine, *plan.clone(), params, ctes)
        }
        uqa_planner::CommandPlan::Delete(plan) => {
            plan.statement_privilege_subject
                .get_or_insert(subject.clone());
            plan.target_privilege_subject.get_or_insert(subject);
            delete::run_delete_with_ctes(engine, *plan.clone(), params, ctes)
        }
        uqa_planner::CommandPlan::Merge(plan) => {
            plan.statement_privilege_subject
                .get_or_insert(subject.clone());
            plan.target_privilege_subject.get_or_insert(subject);
            merge::run_merge_with_ctes(engine, *plan.clone(), params, ctes)
        }
        _ => Err(SQLError::Internal(
            "non-DML command in a WITH definition".into(),
        )),
    }
}

pub(in crate::sql) fn cursor_command_returning_schema(
    engine: &Engine,
    command: &uqa_planner::CommandPlan,
    params: &[SQLParam],
) -> Result<Option<uqa_execution::RowSchema>, SQLError> {
    match command {
        uqa_planner::CommandPlan::Merge(plan) => {
            merge_command_returning_schema(engine, plan, params)
        }
        _ => dml_command_returning_schema(engine, command, params),
    }
}
