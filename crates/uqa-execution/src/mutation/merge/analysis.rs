//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture statement analysis scopes and check target/source read privileges.
use crate::mutation::{command_scope::CommandScopeSource, statement::MutationExecutionContext};
use crate::query::{
    privileges::{
        ensure_select_privileges_for_source_expressions_with_control,
        ensure_target_table_select_for_expressions_with_control,
    },
    CteScope,
};
use uqa_sql::{
    plan::MergePlan,
    semantics::{
        merge::ensure_merge_mutation_privileges_with_control,
        privileges::TargetSelectPrivilegeRequest,
        view_privileges::merge_privilege_expressions_with_control,
    },
    SQLError,
};

pub fn merge_analysis_scope<S: Clone>(
    scopes: &dyn CommandScopeSource<S>,
    stmt: &MergePlan,
    inherited: Option<&CteScope<S>>,
) -> Result<CteScope<S>, SQLError> {
    let mut scope = scopes.command_scope(stmt.statement_privilege_subject.as_ref(), false)?;
    let retained = scopes.read_control()?;
    let control = uqa_core::memory::ProductionControl::new(
        retained.memory(),
        retained.cancellation(),
        scopes.cancellation(),
    );
    if let Some(parent) = inherited {
        scope.inherit_cte_bindings_with_control(parent, &control)?;
    }
    for cte in &stmt.ctes {
        scope.insert_deferred_with_control(
            uqa_sql::plan::RetainedCtePlan::capture(cte, &control)?,
            &control,
        )?;
    }
    scope.scalar_subqueries =
        uqa_sql::plan::RetainedQueryArena::capture(&stmt.subqueries, &control)?;
    Ok(scope)
}

pub(in crate::mutation) fn ensure_merge_privileges<S: Clone + 'static>(
    mutation: &MutationExecutionContext<'_, S>,
    stmt: &MergePlan,
    inherited: Option<&CteScope<S>>,
) -> Result<(), SQLError> {
    let retained = mutation.scopes.read_control()?;
    let control = uqa_core::memory::ProductionControl::new(
        retained.memory(),
        retained.cancellation(),
        mutation.scopes.cancellation(),
    );
    ensure_merge_mutation_privileges_with_control(mutation.privileges, stmt, &control)?;
    let expressions = merge_privilege_expressions_with_control(stmt, &control)?;
    let target_scope = mutation
        .preparation
        .referential
        .assignment
        .scopes
        .current_routine_scope();
    ensure_target_table_select_for_expressions_with_control(
        TargetSelectPrivilegeRequest {
            table: &stmt.target,
            privilege_subject: stmt.target_privilege_subject.as_ref(),
            target_qualifier: &stmt.target_qualifier,
            returning_aliases: &stmt.returning_aliases,
            expressions: &expressions,
            subqueries: &stmt.subqueries,
            required_columns: &[],
        },
        &target_scope,
        &control,
    )?;
    let scope = merge_analysis_scope(mutation.scopes, stmt, inherited)?;
    ensure_select_privileges_for_source_expressions_with_control(
        &stmt.source,
        &expressions,
        &scope,
        &control,
    )
}

/// Reject an INSERT action whose VALUES contain a set-returning call before the merge runs. `PostgreSQL` analyzes that list like a target list, and its executor rejects the call when it initializes the action, whether or not the action ever fires.
pub(super) fn reject_set_valued_inserts(
    routines: &dyn uqa_sql::routines::RoutineResolution,
    stmt: &MergePlan,
) -> Result<(), SQLError> {
    for clause in &stmt.when_clauses {
        if let uqa_sql::plan::MergeWhenPlan::InsertNotMatched { values, .. } = clause {
            if values.iter().any(|value| {
                uqa_sql::semantics::sets::static_setness::expression_may_return_set_statically(
                    routines, value,
                )
            }) {
                return Err(uqa_sql::semantics::sets::placement::set_valued_call_error());
            }
        }
    }
    Ok(())
}

pub(super) fn merge_target_lock_strength(
    catalog: &dyn crate::query::locking::context::LockingCatalog,
    stmt: &MergePlan,
    target_table: &str,
) -> uqa_sql::ast::LockStrength {
    if stmt.when_clauses.iter().any(|clause| {
        matches!(
            clause,
            uqa_sql::plan::MergeWhenPlan::DeleteMatched { .. }
                | uqa_sql::plan::MergeWhenPlan::DeleteNotMatchedBySource { .. }
        )
    }) {
        return uqa_sql::ast::LockStrength::ForUpdate;
    }
    let columns = stmt
        .when_clauses
        .iter()
        .filter_map(|clause| match clause {
            uqa_sql::plan::MergeWhenPlan::UpdateMatched { assignments, .. }
            | uqa_sql::plan::MergeWhenPlan::UpdateNotMatchedBySource { assignments, .. } => {
                Some(assignments)
            }
            _ => None,
        })
        .flatten()
        .flat_map(|assignment| assignment.target.column_names())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if columns.is_empty() {
        uqa_sql::ast::LockStrength::ForUpdate
    } else {
        crate::query::locking::context::update_lock_strength(catalog, target_table, &columns)
    }
}

pub fn merge_command_returning_schema<S: Clone + 'static>(
    returning: &crate::mutation::returning::ReturningExecutionContext<'_, S>,
    scopes: &dyn CommandScopeSource<S>,
    stmt: &MergePlan,
    params: &[uqa_sql::SQLParam],
) -> Result<Option<crate::RowSchema>, SQLError> {
    if stmt.returning.is_empty() {
        return Ok(None);
    }
    let scope = merge_analysis_scope(scopes, stmt, None)?;
    uqa_sql::semantics::merge::merge_command_returning_schema(
        returning.routines,
        returning.catalog,
        returning.rows.relations,
        stmt,
        params,
        &crate::query::binding::binding_context(&scope)?,
    )
}
