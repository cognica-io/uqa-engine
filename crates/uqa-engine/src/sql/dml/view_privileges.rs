//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Privilege checks at each view boundary before DML rewrite or trigger dispatch.

use std::collections::BTreeSet;

use uqa_planner::{
    ConflictActionPlan, ConflictPlan, DeletePlan, InsertPlan, MergePlan, MergeWhenPlan, UpdatePlan,
};
use uqa_sql::SQLError;

use super::{Engine, TargetSelectPrivilegeRequest};
use crate::engine_table_security::TableAclPrivilege;

fn view_target(engine: &Engine, name: &str) -> Result<(crate::StoredView, Vec<String>), SQLError> {
    let view = engine
        .view_definition(name)?
        .ok_or_else(|| SQLError::UnknownTable(name.to_string()))?;
    let columns = view.output_columns.clone().ok_or_else(|| {
        SQLError::Internal(format!(
            "loaded view `{name}` has no durable public column metadata"
        ))
    })?;
    Ok((view, columns))
}

fn privilege_subject(engine: &Engine, rewritten_subject: Option<&str>) -> String {
    rewritten_subject.map_or_else(|| engine.current_user_name(), str::to_string)
}

fn next_privilege_subject(view: &crate::StoredView, subject: String) -> String {
    if view.security_invoker() {
        subject
    } else {
        view.role_owner.clone()
    }
}

fn validate_columns(
    name: &str,
    available: &[String],
    requested: &[String],
) -> Result<(), SQLError> {
    let mut seen = BTreeSet::new();
    for column in requested {
        if !seen.insert(column) {
            return Err(SQLError::Routine {
                sqlstate: "42701".into(),
                message: format!("column \"{column}\" specified more than once"),
            });
        }
        if !available.contains(column) {
            return Err(SQLError::UnknownColumn(format!("{name}.{column}")));
        }
    }
    Ok(())
}

pub(super) fn ensure_insert(engine: &Engine, statement: &InsertPlan) -> Result<String, SQLError> {
    let (view, available) = view_target(engine, &statement.table)?;
    validate_columns(&statement.table, &available, &statement.columns)?;
    let subject = privilege_subject(engine, statement.target_privilege_subject.as_deref());
    let default_values = statement.source.is_none()
        && statement.columns.is_empty()
        && statement.rows.iter().all(Vec::is_empty);
    if default_values {
        engine.ensure_any_view_column_privilege_for(
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
                    crate::sql::select::query_plan_output_columns(source)
                        .map(|columns| columns.len())
                },
            );
            supplied.map_or_else(
                || available.clone(),
                |width| available.iter().take(width).cloned().collect(),
            )
        } else {
            statement.columns.clone()
        };
        for column in columns {
            engine.ensure_view_column_privilege_for(
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
            .map(|assignment| assignment.column.clone())
            .collect::<Vec<_>>();
        validate_columns(&statement.table, &available, &update_columns)?;
        for column in &update_columns {
            engine.ensure_view_column_privilege_for(
                &statement.table,
                &view,
                column,
                &subject,
                TableAclPrivilege::Update,
            )?;
        }
        expressions.extend(assignments.iter().map(|assignment| &assignment.value));
        expressions.extend(predicate.iter().map(Box::as_ref));
        conflict_columns.as_slice()
    } else {
        &[]
    };
    super::ensure_target_table_select_for_expressions(
        engine,
        TargetSelectPrivilegeRequest {
            table: &statement.table,
            privilege_subject: Some(&subject),
            target_qualifier: &statement.target_qualifier,
            returning_aliases: &statement.returning_aliases,
            expressions: &expressions,
            subqueries: &statement.subqueries,
            required_columns,
        },
    )?;
    Ok(next_privilege_subject(&view, subject))
}

pub(super) fn ensure_update(engine: &Engine, statement: &UpdatePlan) -> Result<String, SQLError> {
    let (view, available) = view_target(engine, &statement.table)?;
    let columns = statement
        .assignments
        .iter()
        .map(|assignment| assignment.column.clone())
        .collect::<Vec<_>>();
    validate_columns(&statement.table, &available, &columns)?;
    let subject = privilege_subject(engine, statement.target_privilege_subject.as_deref());
    for column in &columns {
        engine.ensure_view_column_privilege_for(
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
        .map(|assignment| &assignment.value)
        .chain(statement.predicate.iter())
        .chain(
            statement
                .returning
                .iter()
                .map(|projection| &projection.expr),
        )
        .collect::<Vec<_>>();
    super::ensure_target_table_select_for_expressions(
        engine,
        TargetSelectPrivilegeRequest {
            table: &statement.table,
            privilege_subject: Some(&subject),
            target_qualifier: &statement.target_qualifier,
            returning_aliases: &statement.returning_aliases,
            expressions: &expressions,
            subqueries: &statement.subqueries,
            required_columns: &[],
        },
    )?;
    Ok(next_privilege_subject(&view, subject))
}

pub(super) fn ensure_delete(engine: &Engine, statement: &DeletePlan) -> Result<String, SQLError> {
    let (view, _) = view_target(engine, &statement.table)?;
    let subject = privilege_subject(engine, statement.target_privilege_subject.as_deref());
    engine.ensure_view_privilege_for(
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
    super::ensure_target_table_select_for_expressions(
        engine,
        TargetSelectPrivilegeRequest {
            table: &statement.table,
            privilege_subject: Some(&subject),
            target_qualifier: &statement.target_qualifier,
            returning_aliases: &statement.returning_aliases,
            expressions: &expressions,
            subqueries: &statement.subqueries,
            required_columns: &[],
        },
    )?;
    Ok(next_privilege_subject(&view, subject))
}

pub(super) fn ensure_merge(engine: &Engine, statement: &MergePlan) -> Result<String, SQLError> {
    let (view, available) = view_target(engine, &statement.target)?;
    let subject = privilege_subject(engine, statement.target_privilege_subject.as_deref());
    let mut requires_delete = false;
    let mut requires_any_insert = false;
    let mut column_privileges = BTreeSet::new();
    for clause in &statement.when_clauses {
        match clause {
            MergeWhenPlan::InsertNotMatched {
                columns, values, ..
            } => {
                validate_columns(&statement.target, &available, columns)?;
                if columns.is_empty() && values.is_empty() {
                    requires_any_insert = true;
                } else {
                    let columns = if columns.is_empty() {
                        available.iter().take(values.len()).cloned().collect()
                    } else {
                        columns.clone()
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
                    .map(|assignment| assignment.column.clone())
                    .collect::<Vec<_>>();
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
        engine.ensure_view_privilege_for(
            &statement.target,
            &view,
            &subject,
            TableAclPrivilege::Delete,
        )?;
    }
    if requires_any_insert {
        engine.ensure_any_view_column_privilege_for(
            &statement.target,
            &view,
            &subject,
            TableAclPrivilege::Insert,
        )?;
    }
    for (privilege, column) in column_privileges {
        engine.ensure_view_column_privilege_for(
            &statement.target,
            &view,
            &column,
            &subject,
            privilege,
        )?;
    }
    let expressions = super::merge::merge_privilege_expressions(statement);
    super::ensure_target_table_select_for_expressions(
        engine,
        TargetSelectPrivilegeRequest {
            table: &statement.target,
            privilege_subject: Some(&subject),
            target_qualifier: &statement.target_qualifier,
            returning_aliases: &statement.returning_aliases,
            expressions: &expressions,
            subqueries: &statement.subqueries,
            required_columns: &[],
        },
    )?;
    Ok(next_privilege_subject(&view, subject))
}
