//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL DML execution, constraints, referential actions, and RETURNING rows.

use super::{
    build_join_spill_with_ctes, build_projection_physical_row_with_ctes, partition_insert_target,
    BTreeMap, BTreeSet, ColumnType, CteScope, DocId, Document, Engine, ForeignKey, SQLError,
    SQLParam, SQLResult, Value,
};
use uqa_execution::{OwnedPhysicalRow, PhysicalRow, RowSchema, ScalarExpr};
use uqa_planner::{
    ConflictActionPlan, ConflictPlan, DeletePlan, InsertPlan, MergePlan, MergeWhenPlan,
    ProjectionPlan, UpdatePlan, ViewCheckPlan,
};

mod protocol;
mod view_rules;

pub(in crate::sql) use protocol::*;
pub(crate) use protocol::{
    CommandExactIndex, CommandMutationOverlay, CommandStoredDocument, DeferredForeignKeyCheck,
    TransactionRowChange,
};
use view_rules::{prepare_view_rule_batches, ViewRuleBatchRequest};

pub(in crate::sql) use uqa_planner::mutation_outputs::prune_unused_query_outputs;

use uqa_execution::mutation::errors::dml_storage_error;

/// Resolve a statement's mutation target once, before any internal storage or rewrite path can observe its textual name.
pub(in crate::sql) fn resolve_dml_target_name(
    engine: &Engine,
    name: &str,
    target_relation_bound: bool,
) -> Result<String, SQLError> {
    engine.resolve_mutation_target_name(name, target_relation_bound)
}

pub(super) use uqa_sql::semantics::privileges::TargetSelectPrivilegeRequest;

pub(super) fn ensure_target_table_select_for_expressions(
    engine: &Engine,
    request: TargetSelectPrivilegeRequest<'_, '_>,
) -> Result<(), SQLError> {
    let mut scope = crate::capabilities::query_scope::new_for_current_routine(engine);
    uqa_execution::query::privileges::ensure_target_table_select_for_expressions(
        request, &mut scope,
    )
}

pub(crate) fn update_lock_strength(
    engine: &Engine,
    table: &str,
    columns: &[String],
) -> uqa_sql::ast::LockStrength {
    let Ok(keys) = engine.referenceable_keys(table) else {
        return uqa_sql::ast::LockStrength::ForUpdate;
    };
    let Ok(Some(definitions)) = engine.try_describe_table(table) else {
        return uqa_sql::ast::LockStrength::ForUpdate;
    };
    uqa_sql::semantics::locking::update_lock_strength(&keys, &definitions, columns)
}

use uqa_execution::mutation::errors::missing_document_error;

fn dml_target_row_for_storage(
    engine: &Engine,
    table: &str,
    storage_table: &str,
    qualifier: &str,
    doc_id: DocId,
    document: &Document,
) -> Result<OwnedPhysicalRow, SQLError> {
    uqa_execution::mutation::rows::target_row_for_storage(
        engine.mutation_row_context(),
        table,
        storage_table,
        qualifier,
        doc_id,
        document,
    )
}

pub(in crate::sql) fn existing_tuple_metadata(
    engine: &Engine,
    table: &str,
    doc_id: DocId,
) -> Result<uqa_storage::DocumentMetadata, SQLError> {
    uqa_execution::mutation::rows::existing_tuple_metadata(
        engine.mutation_row_context(),
        table,
        doc_id,
    )
}

pub(in crate::sql) fn new_tuple_metadata(
    engine: &Engine,
) -> Result<uqa_storage::DocumentMetadata, SQLError> {
    uqa_execution::mutation::rows::new_tuple_metadata(engine.mutation_row_context())
}

use uqa_execution::mutation::assignment::validate_view_checks;
type ViewCheckContext<'a> = uqa_execution::mutation::assignment::ViewCheckContext<
    'a,
    crate::session::StatementReadSnapshot,
>;

fn dml_null_target_row(
    engine: &Engine,
    table: &str,
    qualifier: &str,
) -> Result<OwnedPhysicalRow, SQLError> {
    uqa_execution::mutation::rows::null_target_row(engine.mutation_row_context(), table, qualifier)
}

use uqa_execution::mutation::rows::join_rows as dml_join_rows;

use uqa_sql::semantics::mutation_qualifiers::validate_dml_expression_qualifiers;

fn insert_identity_columns(
    engine: &Engine,
    table: &str,
    action: &str,
) -> Result<(Option<String>, String, bool), SQLError> {
    uqa_execution::mutation::identity::insert_identity_columns(
        engine.insert_identity_context(),
        table,
        action,
    )
}
fn prepare_auto_increment_identity(
    engine: &Engine,
    table: &str,
    id_column: &str,
    auto_id_column: Option<&str>,
    document: &mut Document,
    action: &str,
) -> Result<Option<(DocId, bool)>, SQLError> {
    uqa_execution::mutation::identity::prepare_auto_increment_identity(
        engine.insert_identity_context(),
        table,
        id_column,
        auto_id_column,
        document,
        action,
    )
}
fn persist_auto_increment_identity(
    engine: &Engine,
    table: &str,
    auto_id_column: Option<&str>,
    action: &str,
) -> Result<(), SQLError> {
    uqa_execution::mutation::identity::persist_auto_increment_identity(
        engine.insert_identity_context(),
        table,
        auto_id_column,
        action,
    )
}
fn prepare_insert_identity(
    engine: &Engine,
    allocation_table: &str,
    id_column: &str,
    accepts_supplied_identity: bool,
    auto_id_column: Option<&str>,
    document: &mut Document,
    action: &str,
) -> Result<(DocId, bool), SQLError> {
    uqa_execution::mutation::identity::prepare_insert_identity(
        engine.insert_identity_context(),
        allocation_table,
        id_column,
        accepts_supplied_identity,
        auto_id_column,
        document,
        action,
    )
}
fn validate_mutation_columns<'a>(
    engine: &Engine,
    table: &str,
    columns: impl IntoIterator<Item = &'a str>,
    action: &str,
) -> Result<(), SQLError> {
    uqa_sql::assignment::columns::validate_mutation_columns(engine, table, columns, action)
}

fn eval_mutation_expr(
    engine: &Engine,
    ctes: &CteScope,
    expression: &ScalarExpr,
    row: Option<&OwnedPhysicalRow>,
    params: &[SQLParam],
) -> Result<Value, SQLError> {
    uqa_execution::mutation::expressions::eval_mutation_expr(
        engine.mutation_expression_context(),
        ctes,
        expression,
        row,
        params,
    )
}

fn row_independent_mutation_qualification_count(
    engine: &Engine,
    predicate: Option<&ScalarExpr>,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<Option<usize>, SQLError> {
    uqa_execution::mutation::expressions::row_independent_mutation_qualification_count(
        engine.mutation_expression_context(),
        predicate,
        params,
        ctes,
    )
}

pub(in crate::sql) use uqa_execution::mutation::assignment::MutationAssignmentTarget;
fn eval_mutation_assignment(
    engine: &Engine,
    ctes: &CteScope,
    target: MutationAssignmentTarget<'_>,
    expression: &ScalarExpr,
    row: Option<&OwnedPhysicalRow>,
    params: &[SQLParam],
) -> Result<Option<Value>, SQLError> {
    uqa_execution::mutation::assignment::eval_mutation_assignment(
        engine.mutation_assignment_context(),
        ctes,
        target,
        expression,
        row,
        params,
    )
}
mod conflict;
mod constraints;
mod delete;
mod insert;
mod merge;
mod update;
mod vectors;
pub(in crate::sql) mod view_automatic;
mod view_privileges;
mod view_triggers;

pub(in crate::sql) use conflict::*;
pub(crate) use constraints::*;
pub(in crate::sql) use delete::*;
pub(in crate::sql) use insert::*;
pub(in crate::sql) use merge::*;
pub(in crate::sql) use update::*;
pub(in crate::sql) use vectors::*;

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
