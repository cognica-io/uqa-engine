//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture statement analysis scopes and check target/source read privileges.
use crate::mutation::{command_scope::CommandScopeSource, statement::MutationExecutionContext};
use crate::query::{
    privileges::{
        ensure_select_privileges_for_source_expressions, ensure_target_table_select_for_expressions,
    },
    CteScope,
};
use uqa_sql::{
    plan::MergePlan,
    semantics::{
        merge::ensure_merge_mutation_privileges, privileges::TargetSelectPrivilegeRequest,
        view_privileges::merge_privilege_expressions,
    },
    SQLError,
};

pub fn merge_analysis_scope<S: Clone>(
    scopes: &dyn CommandScopeSource<S>,
    stmt: &MergePlan,
    inherited: Option<&CteScope<S>>,
) -> Result<CteScope<S>, SQLError> {
    let mut scope = scopes.command_scope(stmt.statement_privilege_subject.as_deref(), false)?;
    if let Some(parent) = inherited {
        scope.inherit_cte_bindings(parent);
    }
    for cte in &stmt.ctes {
        scope.insert_deferred(cte.clone());
    }
    scope.scalar_subqueries.clone_from(&stmt.subqueries);
    Ok(scope)
}

pub(super) fn ensure_merge_privileges<S: Clone + 'static>(
    mutation: &MutationExecutionContext<'_, S>,
    stmt: &MergePlan,
    inherited: Option<&CteScope<S>>,
) -> Result<(), SQLError> {
    ensure_merge_mutation_privileges(mutation.privileges, stmt)?;
    let expressions = merge_privilege_expressions(stmt);
    let mut target_scope = mutation
        .preparation
        .referential
        .assignment
        .scopes
        .current_routine_scope();
    ensure_target_table_select_for_expressions(
        TargetSelectPrivilegeRequest {
            table: &stmt.target,
            privilege_subject: stmt.target_privilege_subject.as_deref(),
            target_qualifier: &stmt.target_qualifier,
            returning_aliases: &stmt.returning_aliases,
            expressions: &expressions,
            subqueries: &stmt.subqueries,
            required_columns: &[],
        },
        &mut target_scope,
    )?;
    let scope = merge_analysis_scope(mutation.scopes, stmt, inherited)?;
    ensure_select_privileges_for_source_expressions(&stmt.source, &expressions, &scope)
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
        .map(|assignment| assignment.column.clone())
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
