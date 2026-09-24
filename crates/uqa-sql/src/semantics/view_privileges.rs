//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Privilege checks at each view boundary before DML rewrite or trigger dispatch.

use crate::catalog::roles::RoleReference;
use std::collections::BTreeSet;

use crate::plan::{
    ConflictActionPlan, ConflictPlan, DeletePlan, InsertPlan, MergePlan, MergeWhenPlan, UpdatePlan,
};
use crate::SQLError;

use super::privileges::TargetSelectPrivilegeRequest;
use crate::catalog::security::table::TableAclPrivilege;
use crate::catalog::stored_view::StoredView;

fn view_target(
    services: &dyn ViewPrivilegeCatalog,
    name: &str,
) -> Result<(StoredView, Vec<String>), SQLError> {
    let view = services
        .view_definition(name)?
        .ok_or_else(|| SQLError::UnknownTable(name.to_string()))?;
    let columns = view.output_columns.clone().ok_or_else(|| {
        SQLError::Internal(format!(
            "loaded view `{name}` has no durable public column metadata"
        ))
    })?;
    Ok((view, columns))
}

fn privilege_subject(
    services: &dyn ViewPrivilegeCatalog,
    rewritten_subject: Option<&RoleReference>,
) -> RoleReference {
    rewritten_subject.map_or_else(|| services.current_role(), Clone::clone)
}

fn next_privilege_subject(
    services: &dyn ViewPrivilegeCatalog,
    view: &StoredView,
    subject: RoleReference,
) -> Result<RoleReference, SQLError> {
    if view.security_invoker() {
        Ok(subject)
    } else {
        services.bound_role(view.security.role_owner)
    }
}

fn validate_columns(
    name: &str,
    available: &[String],
    requested: &[String],
) -> Result<(), SQLError> {
    for column in requested {
        if !available.contains(column) {
            return Err(SQLError::UnknownColumn(format!("{name}.{column}")));
        }
    }
    Ok(())
}

fn validate_insert_columns(statement: &InsertPlan, available: &[String]) -> Result<(), SQLError> {
    crate::assignment::targets::validate_repeated_targets(&statement.columns, true)?;
    validate_columns(
        &statement.table,
        available,
        &statement
            .columns
            .iter()
            .map(|target| target.column.clone())
            .collect::<Vec<_>>(),
    )
}

pub fn ensure_insert(
    services: &dyn ViewPrivilegeCatalog,
    statement: &InsertPlan,
) -> Result<RoleReference, SQLError> {
    let (view, available) = view_target(services, &statement.table)?;
    validate_insert_columns(statement, &available)?;
    let subject = privilege_subject(services, statement.target_privilege_subject.as_ref());
    let default_values = statement.source.is_none()
        && statement.columns.is_empty()
        && statement.rows.iter().all(Vec::is_empty);
    if default_values {
        services.ensure_any_view_column_privilege_for(
            &statement.table,
            &view,
            &subject,
            TableAclPrivilege::Insert,
        )?;
    } else {
        let columns = if statement.columns.is_empty() {
            let supplied = statement.source.as_deref().map_or_else(
                || statement.rows.first().map(Vec::len),
                |source| {
                    crate::semantics::projection::query_plan_output_columns(source)
                        .map(|columns| columns.len())
                },
            );
            supplied.map_or_else(
                || available.clone(),
                |width| available.iter().take(width).cloned().collect(),
            )
        } else {
            statement
                .columns
                .iter()
                .map(|target| target.column.clone())
                .collect()
        };
        for column in columns {
            services.ensure_view_column_privilege_for(
                &statement.table,
                &view,
                &column,
                &subject,
                TableAclPrivilege::Insert,
            )?;
        }
    }
    let mut expressions = statement
        .returning
        .iter()
        .map(|projection| &projection.expr)
        .collect::<Vec<_>>();
    if let Some(conflict) = &statement.on_conflict {
        expressions.extend(conflict.expressions.iter());
        expressions.extend(conflict.predicate.iter().map(Box::as_ref));
    }
    let required_columns = if let Some(ConflictPlan {
        conflict_columns,
        action:
            ConflictActionPlan::Update {
                assignments,
                predicate,
            },
        ..
    }) = statement.on_conflict.as_ref()
    {
        let update_columns = assignments
            .iter()
            .map(|assignment| assignment.target.column.clone())
            .collect::<Vec<_>>();
        crate::assignment::targets::validate_repeated_targets(
            assignments.iter().map(|assignment| &assignment.target),
            false,
        )?;
        validate_columns(&statement.table, &available, &update_columns)?;
        for column in &update_columns {
            services.ensure_view_column_privilege_for(
                &statement.table,
                &view,
                column,
                &subject,
                TableAclPrivilege::Update,
            )?;
        }
        expressions.extend(
            assignments
                .iter()
                .flat_map(crate::plan::AssignmentPlan::expressions),
        );
        expressions.extend(predicate.iter().map(Box::as_ref));
        conflict_columns.as_slice()
    } else {
        &[]
    };
    services.ensure_target_select(TargetSelectPrivilegeRequest {
        table: &statement.table,
        privilege_subject: Some(&subject),
        target_qualifier: &statement.target_qualifier,
        returning_aliases: &statement.returning_aliases,
        expressions: &expressions,
        subqueries: &statement.subqueries,
        required_columns,
    })?;
    next_privilege_subject(services, &view, subject)
}

pub fn ensure_update(
    services: &dyn ViewPrivilegeCatalog,
    statement: &UpdatePlan,
) -> Result<RoleReference, SQLError> {
    let (view, available) = view_target(services, &statement.table)?;
    let columns = statement
        .assignments
        .iter()
        .map(|assignment| assignment.target.column.clone())
        .collect::<Vec<_>>();
    crate::assignment::targets::validate_repeated_targets(
        statement
            .assignments
            .iter()
            .map(|assignment| &assignment.target),
        false,
    )?;
    validate_columns(&statement.table, &available, &columns)?;
    let subject = privilege_subject(services, statement.target_privilege_subject.as_ref());
    for column in &columns {
        services.ensure_view_column_privilege_for(
            &statement.table,
            &view,
            column,
            &subject,
            TableAclPrivilege::Update,
        )?;
    }
    let expressions = statement
        .assignments
        .iter()
        .flat_map(crate::plan::AssignmentPlan::expressions)
        .chain(statement.predicate.iter())
        .chain(
            statement
                .returning
                .iter()
                .map(|projection| &projection.expr),
        )
        .collect::<Vec<_>>();
    services.ensure_target_select(TargetSelectPrivilegeRequest {
        table: &statement.table,
        privilege_subject: Some(&subject),
        target_qualifier: &statement.target_qualifier,
        returning_aliases: &statement.returning_aliases,
        expressions: &expressions,
        subqueries: &statement.subqueries,
        required_columns: &[],
    })?;
    next_privilege_subject(services, &view, subject)
}

pub fn ensure_delete(
    services: &dyn ViewPrivilegeCatalog,
    statement: &DeletePlan,
) -> Result<RoleReference, SQLError> {
    let (view, _) = view_target(services, &statement.table)?;
    let subject = privilege_subject(services, statement.target_privilege_subject.as_ref());
    services.ensure_view_privilege_for(
        &statement.table,
        &view,
        &subject,
        TableAclPrivilege::Delete,
    )?;
    let expressions = statement
        .predicate
        .iter()
        .chain(
            statement
                .returning
                .iter()
                .map(|projection| &projection.expr),
        )
        .collect::<Vec<_>>();
    services.ensure_target_select(TargetSelectPrivilegeRequest {
        table: &statement.table,
        privilege_subject: Some(&subject),
        target_qualifier: &statement.target_qualifier,
        returning_aliases: &statement.returning_aliases,
        expressions: &expressions,
        subqueries: &statement.subqueries,
        required_columns: &[],
    })?;
    next_privilege_subject(services, &view, subject)
}

pub fn ensure_merge(
    services: &dyn ViewPrivilegeCatalog,
    statement: &MergePlan,
) -> Result<RoleReference, SQLError> {
    let (view, available) = view_target(services, &statement.target)?;
    let subject = privilege_subject(services, statement.target_privilege_subject.as_ref());
    let mut requires_delete = false;
    let mut requires_any_insert = false;
    let mut column_privileges = BTreeSet::new();
    for clause in &statement.when_clauses {
        match clause {
            MergeWhenPlan::InsertNotMatched {
                columns, values, ..
            } => {
                crate::assignment::targets::validate_repeated_targets(columns, true)?;
                validate_columns(
                    &statement.target,
                    &available,
                    &columns
                        .iter()
                        .map(|target| target.column.clone())
                        .collect::<Vec<_>>(),
                )?;
                if columns.is_empty() && values.is_empty() {
                    requires_any_insert = true;
                } else {
                    let columns: Vec<String> = if columns.is_empty() {
                        available.iter().take(values.len()).cloned().collect()
                    } else {
                        columns.iter().map(|target| target.column.clone()).collect()
                    };
                    column_privileges.extend(
                        columns
                            .into_iter()
                            .map(|column| (TableAclPrivilege::Insert, column)),
                    );
                }
            }
            MergeWhenPlan::UpdateMatched { assignments, .. }
            | MergeWhenPlan::UpdateNotMatchedBySource { assignments, .. } => {
                let columns = assignments
                    .iter()
                    .map(|assignment| assignment.target.column.clone())
                    .collect::<Vec<_>>();
                crate::assignment::targets::validate_repeated_targets(
                    assignments.iter().map(|assignment| &assignment.target),
                    false,
                )?;
                validate_columns(&statement.target, &available, &columns)?;
                column_privileges.extend(
                    columns
                        .into_iter()
                        .map(|column| (TableAclPrivilege::Update, column)),
                );
            }
            MergeWhenPlan::DeleteMatched { .. }
            | MergeWhenPlan::DeleteNotMatchedBySource { .. } => requires_delete = true,
            _ => {}
        }
    }
    if requires_delete {
        services.ensure_view_privilege_for(
            &statement.target,
            &view,
            &subject,
            TableAclPrivilege::Delete,
        )?;
    }
    if requires_any_insert {
        services.ensure_any_view_column_privilege_for(
            &statement.target,
            &view,
            &subject,
            TableAclPrivilege::Insert,
        )?;
    }
    for (privilege, column) in column_privileges {
        services.ensure_view_column_privilege_for(
            &statement.target,
            &view,
            &column,
            &subject,
            privilege,
        )?;
    }
    let expressions = merge_privilege_expressions(statement);
    services.ensure_target_select(TargetSelectPrivilegeRequest {
        table: &statement.target,
        privilege_subject: Some(&subject),
        target_qualifier: &statement.target_qualifier,
        returning_aliases: &statement.returning_aliases,
        expressions: &expressions,
        subqueries: &statement.subqueries,
        required_columns: &[],
    })?;
    next_privilege_subject(services, &view, subject)
}

pub fn merge_privilege_expressions(stmt: &MergePlan) -> Vec<&crate::ScalarExpr> {
    let mut expressions = vec![&stmt.join_condition];
    expressions.extend(stmt.target_predicate.iter());
    expressions.extend(stmt.returning.iter().map(|projection| &projection.expr));
    for clause in &stmt.when_clauses {
        match clause {
            MergeWhenPlan::UpdateMatched {
                condition,
                assignments,
            }
            | MergeWhenPlan::UpdateNotMatchedBySource {
                condition,
                assignments,
            } => {
                expressions.extend(condition.iter());
                expressions.extend(
                    assignments
                        .iter()
                        .flat_map(crate::plan::AssignmentPlan::expressions),
                );
            }
            MergeWhenPlan::InsertNotMatched {
                condition,
                columns,
                values,
            } => {
                expressions.extend(condition.iter());
                expressions.extend(
                    columns
                        .iter()
                        .flat_map(crate::ast::AssignmentTarget::expressions),
                );
                expressions.extend(values);
            }
            MergeWhenPlan::DeleteMatched { condition }
            | MergeWhenPlan::DeleteNotMatchedBySource { condition }
            | MergeWhenPlan::NothingMatched { condition }
            | MergeWhenPlan::NothingNotMatched { condition }
            | MergeWhenPlan::NothingNotMatchedBySource { condition } => {
                expressions.extend(condition.iter());
            }
        }
    }
    expressions
}

/// Authorization access to one loaded view and the current statement's SELECT privileges.
pub trait ViewPrivilegeCatalog {
    fn view_definition(&self, name: &str) -> Result<Option<StoredView>, SQLError>;
    fn current_role(&self) -> RoleReference;
    fn bound_role(
        &self,
        identity: crate::catalog::roles::RoleIdentity,
    ) -> Result<RoleReference, SQLError>;
    fn ensure_view_privilege_for(
        &self,
        name: &str,
        view: &StoredView,
        subject: &RoleReference,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError>;
    fn ensure_view_column_privilege_for(
        &self,
        name: &str,
        view: &StoredView,
        column: &str,
        subject: &RoleReference,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError>;
    fn ensure_any_view_column_privilege_for(
        &self,
        name: &str,
        view: &StoredView,
        subject: &RoleReference,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError>;
    fn ensure_target_select(
        &self,
        request: TargetSelectPrivilegeRequest<'_, '_>,
    ) -> Result<(), SQLError>;
}
