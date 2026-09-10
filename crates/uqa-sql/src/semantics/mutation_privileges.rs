//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mutation target privilege analysis.
use crate::{
    catalog::security::table::TableAclPrivilege,
    plan::UpdatePlan,
    semantics::{privileges::TargetSelectPrivilegeRequest, view_privileges::ViewPrivilegeCatalog},
    SQLError, ScalarExpr,
};
pub trait MutationPrivilegeCatalog: ViewPrivilegeCatalog {
    fn bound_table_column_names(&self, table: &str) -> Result<Vec<String>, SQLError>;
    fn ensure_table_privilege_for(
        &self,
        table: &str,
        subject: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError>;
    fn ensure_column_privilege_for(
        &self,
        table: &str,
        column: &str,
        subject: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError>;
    fn ensure_any_column_privilege_for(
        &self,
        table: &str,
        subject: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError>;
}
pub fn ensure_update_target_privileges<'a>(
    catalog: &dyn MutationPrivilegeCatalog,
    statement: &'a UpdatePlan,
) -> Result<Vec<&'a ScalarExpr>, SQLError> {
    let privilege_subject = statement
        .target_privilege_subject
        .clone()
        .unwrap_or_else(|| catalog.current_user_name());
    for assignment in &statement.assignments {
        catalog.ensure_column_privilege_for(
            &statement.table,
            &assignment.column,
            &privilege_subject,
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
    catalog.ensure_target_select(TargetSelectPrivilegeRequest {
        table: &statement.table,
        privilege_subject: statement.target_privilege_subject.as_deref(),
        target_qualifier: &statement.target_qualifier,
        returning_aliases: &statement.returning_aliases,
        expressions: &expressions,
        subqueries: &statement.subqueries,
        required_columns: &[],
    })?;
    Ok(expressions)
}

/// Analyze INSERT and ON CONFLICT privileges before any trigger or row mutation.
pub fn ensure_insert_target_privileges(
    catalog: &dyn MutationPrivilegeCatalog,
    stmt: &crate::plan::InsertPlan,
    conflict_update_columns: Option<&[String]>,
) -> Result<(), SQLError> {
    use crate::plan::{ConflictActionPlan, ConflictPlan};
    let default_values =
        stmt.source.is_none() && stmt.columns.is_empty() && stmt.rows.iter().all(Vec::is_empty);
    let privilege_subject = stmt
        .target_privilege_subject
        .clone()
        .unwrap_or_else(|| catalog.current_user_name());
    if default_values {
        catalog.ensure_any_column_privilege_for(
            &stmt.table,
            &privilege_subject,
            TableAclPrivilege::Insert,
        )?;
    } else {
        let insert_columns = if stmt.columns.is_empty() {
            let supplied = stmt.source.as_deref().map_or_else(
                || stmt.rows.first().map(Vec::len),
                |source| {
                    crate::semantics::projection::query_plan_output_columns(source)
                        .map(|columns| columns.len())
                },
            );
            let columns = catalog.bound_table_column_names(&stmt.table)?;
            match supplied {
                Some(supplied) => columns.into_iter().take(supplied).collect(),
                None => columns,
            }
        } else {
            stmt.columns.clone()
        };
        for column in insert_columns {
            catalog.ensure_column_privilege_for(
                &stmt.table,
                &column,
                &privilege_subject,
                TableAclPrivilege::Insert,
            )?;
        }
    }
    if let Some(columns) = conflict_update_columns {
        for column in columns {
            catalog.ensure_column_privilege_for(
                &stmt.table,
                column,
                &privilege_subject,
                TableAclPrivilege::Update,
            )?;
        }
    }
    let mut privilege_expressions = stmt
        .returning
        .iter()
        .map(|projection| &projection.expr)
        .collect::<Vec<_>>();
    if let Some(conflict) = &stmt.on_conflict {
        privilege_expressions.extend(conflict.expressions.iter());
        privilege_expressions.extend(conflict.predicate.iter().map(Box::as_ref));
    }
    if let Some(ConflictPlan {
        action:
            ConflictActionPlan::Update {
                assignments,
                predicate,
            },
        ..
    }) = stmt.on_conflict.as_ref()
    {
        privilege_expressions.extend(assignments.iter().map(|assignment| &assignment.value));
        privilege_expressions.extend(predicate.iter().map(Box::as_ref));
    }
    catalog.ensure_target_select(TargetSelectPrivilegeRequest {
        table: &stmt.table,
        privilege_subject: stmt.target_privilege_subject.as_deref(),
        target_qualifier: &stmt.target_qualifier,
        returning_aliases: &stmt.returning_aliases,
        expressions: &privilege_expressions,
        subqueries: &stmt.subqueries,
        required_columns: stmt
            .on_conflict
            .as_ref()
            .map_or(&[][..], |conflict| conflict.conflict_columns.as_slice()),
    })?;
    Ok(())
}

/// Fill missing DML privilege subjects from the surrounding WITH definition.
pub fn inherit_command_privilege_subject(
    command: &mut crate::plan::CommandPlan,
    subject: String,
) -> Result<(), SQLError> {
    use crate::plan::CommandPlan;
    let (statement_subject, target_subject) = match command {
        CommandPlan::Insert(plan) => (
            &mut plan.statement_privilege_subject,
            &mut plan.target_privilege_subject,
        ),
        CommandPlan::Update(plan) => (
            &mut plan.statement_privilege_subject,
            &mut plan.target_privilege_subject,
        ),
        CommandPlan::Delete(plan) => (
            &mut plan.statement_privilege_subject,
            &mut plan.target_privilege_subject,
        ),
        CommandPlan::Merge(plan) => (
            &mut plan.statement_privilege_subject,
            &mut plan.target_privilege_subject,
        ),
        _ => {
            return Err(SQLError::Internal(
                "non-DML command in a WITH definition".into(),
            ))
        }
    };
    statement_subject.get_or_insert(subject.clone());
    target_subject.get_or_insert(subject);
    Ok(())
}
