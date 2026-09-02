//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Privilege analysis performed before MERGE begins mutation work.

use super::{BTreeSet, CteScope, Engine, MergePlan, MergeWhenPlan, SQLError};

fn ensure_merge_mutation_privileges(engine: &Engine, stmt: &MergePlan) -> Result<(), SQLError> {
    let mut column_privileges = BTreeSet::new();
    let mut requires_delete = false;
    let mut requires_any_insert = false;
    let table_columns = engine.bound_table_column_names(&stmt.target)?;
    let privilege_subject = stmt
        .target_privilege_subject
        .clone()
        .unwrap_or_else(|| engine.current_user_name());
    for clause in &stmt.when_clauses {
        match clause {
            MergeWhenPlan::InsertNotMatched {
                columns, values, ..
            } => {
                if columns.is_empty() && values.is_empty() {
                    requires_any_insert = true;
                } else {
                    let columns = if columns.is_empty() {
                        table_columns
                            .iter()
                            .take(values.len())
                            .cloned()
                            .collect::<Vec<_>>()
                    } else {
                        columns.clone()
                    };
                    column_privileges.extend(columns.into_iter().map(|column| {
                        (
                            crate::engine_table_security::TableAclPrivilege::Insert,
                            column,
                        )
                    }));
                }
            }
            MergeWhenPlan::UpdateMatched { assignments, .. }
            | MergeWhenPlan::UpdateNotMatchedBySource { assignments, .. } => {
                column_privileges.extend(assignments.iter().map(|assignment| {
                    (
                        crate::engine_table_security::TableAclPrivilege::Update,
                        assignment.column.clone(),
                    )
                }));
            }
            MergeWhenPlan::DeleteMatched { .. }
            | MergeWhenPlan::DeleteNotMatchedBySource { .. } => requires_delete = true,
            _ => {}
        }
    }
    if requires_delete {
        engine.ensure_table_privilege_for(
            &stmt.target,
            &privilege_subject,
            crate::engine_table_security::TableAclPrivilege::Delete,
        )?;
    }
    if requires_any_insert {
        engine.ensure_any_column_privilege_for(
            &stmt.target,
            &privilege_subject,
            crate::engine_table_security::TableAclPrivilege::Insert,
        )?;
    }
    for (privilege, column) in column_privileges {
        engine.ensure_column_privilege_for(&stmt.target, &column, &privilege_subject, privilege)?;
    }
    Ok(())
}

pub(super) fn ensure_merge_privileges(engine: &Engine, stmt: &MergePlan) -> Result<(), SQLError> {
    ensure_merge_mutation_privileges(engine, stmt)?;
    let privilege_expressions = super::super::merge_privilege_expressions(stmt);
    super::super::super::ensure_target_table_select_for_expressions(
        engine,
        super::super::super::TargetSelectPrivilegeRequest {
            table: &stmt.target,
            privilege_subject: stmt.target_privilege_subject.as_deref(),
            target_qualifier: &stmt.target_qualifier,
            returning_aliases: &stmt.returning_aliases,
            expressions: &privilege_expressions,
            subqueries: &stmt.subqueries,
            required_columns: &[],
        },
    )?;
    let mut ctes = CteScope::new_for_current_routine(engine);
    ctes.scalar_subqueries.clone_from(&stmt.subqueries);
    crate::sql::select::ensure_select_privileges_for_source_expressions(
        &stmt.source,
        &privilege_expressions,
        &ctes,
    )
}
