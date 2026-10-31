//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected mutation subjects, target names and expression references retain the invoking allowance.

use super::{with_subject, MutationPrivilegeCatalog};
use crate::{
    catalog::security::table::TableAclPrivilege,
    plan::{AssignmentPlan, ConflictActionPlan, DeletePlan, InsertPlan, UpdatePlan},
    semantics::privileges::TargetSelectPrivilegeRequest,
    SQLError, ScalarExpr,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

pub fn ensure_update_target_privileges_with_control<'a>(
    catalog: &dyn MutationPrivilegeCatalog,
    statement: &'a UpdatePlan,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<&'a ScalarExpr>>, SQLError> {
    with_subject(
        catalog,
        statement.target_privilege_subject.as_ref(),
        control,
        |subject| {
            for target in statement
                .assignments
                .iter()
                .flat_map(|assignment| assignment.target.targets())
            {
                catalog.ensure_column_privilege_for_with_control(
                    &statement.table,
                    &target.column,
                    subject,
                    TableAclPrivilege::Update,
                    control,
                )?;
            }
            let mut expressions = ProductionVec::new(*control);
            for expression in statement
                .assignments
                .iter()
                .flat_map(AssignmentPlan::expressions)
                .chain(statement.predicate.iter())
                .chain(
                    statement
                        .returning
                        .iter()
                        .map(|projection| &projection.expr),
                )
            {
                expressions.push_copy(expression)?;
            }
            catalog.ensure_target_select_with_control(
                TargetSelectPrivilegeRequest {
                    table: &statement.table,
                    privilege_subject: statement.target_privilege_subject.as_ref(),
                    target_qualifier: &statement.target_qualifier,
                    returning_aliases: &statement.returning_aliases,
                    expressions: &expressions,
                    subqueries: &statement.subqueries,
                    required_columns: &[],
                },
                control,
            )?;
            Ok(expressions.finish()?)
        },
    )
}

pub fn ensure_insert_target_privileges_with_control(
    catalog: &dyn MutationPrivilegeCatalog,
    statement: &InsertPlan,
    conflict_update_columns: Option<&[String]>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let default_values = statement.source.is_none()
        && statement.columns.is_empty()
        && statement.rows.iter().all(Vec::is_empty);
    with_subject(
        catalog,
        statement.target_privilege_subject.as_ref(),
        control,
        |subject| {
            if default_values {
                catalog.ensure_any_column_privilege_for_with_control(
                    &statement.table,
                    subject,
                    TableAclPrivilege::Insert,
                    control,
                )?;
            } else {
                let columns = insert_target_columns_with_control(catalog, statement, control)?;
                for column in columns.iter() {
                    catalog.ensure_column_privilege_for_with_control(
                        &statement.table,
                        column,
                        subject,
                        TableAclPrivilege::Insert,
                        control,
                    )?;
                }
            }
            if let Some(columns) = conflict_update_columns {
                for column in columns {
                    catalog.ensure_column_privilege_for_with_control(
                        &statement.table,
                        column,
                        subject,
                        TableAclPrivilege::Update,
                        control,
                    )?;
                }
            }
            let mut expressions = ProductionVec::new(*control);
            for projection in &statement.returning {
                expressions.push_copy(&projection.expr)?;
            }
            if let Some(conflict) = &statement.on_conflict {
                for expression in conflict
                    .expressions
                    .iter()
                    .chain(conflict.predicate.iter().map(Box::as_ref))
                {
                    expressions.push_copy(expression)?;
                }
                if let ConflictActionPlan::Update {
                    assignments,
                    predicate,
                } = &conflict.action
                {
                    for expression in assignments
                        .iter()
                        .flat_map(AssignmentPlan::expressions)
                        .chain(predicate.iter().map(Box::as_ref))
                    {
                        expressions.push_copy(expression)?;
                    }
                }
            }
            catalog.ensure_target_select_with_control(
                TargetSelectPrivilegeRequest {
                    table: &statement.table,
                    privilege_subject: statement.target_privilege_subject.as_ref(),
                    target_qualifier: &statement.target_qualifier,
                    returning_aliases: &statement.returning_aliases,
                    expressions: &expressions,
                    subqueries: &statement.subqueries,
                    required_columns: statement
                        .on_conflict
                        .as_ref()
                        .map_or(&[][..], |conflict| conflict.conflict_columns.as_slice()),
                },
                control,
            )
        },
    )
}

pub fn ensure_delete_target_privileges_with_control<'a>(
    catalog: &dyn MutationPrivilegeCatalog,
    statement: &'a DeletePlan,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<&'a ScalarExpr>>, SQLError> {
    with_subject(
        catalog,
        statement.target_privilege_subject.as_ref(),
        control,
        |subject| {
            catalog.ensure_table_privilege_for_with_control(
                &statement.table,
                subject,
                TableAclPrivilege::Delete,
                control,
            )?;
            let mut expressions = ProductionVec::new(*control);
            for expression in statement.predicate.iter().chain(
                statement
                    .returning
                    .iter()
                    .map(|projection| &projection.expr),
            ) {
                expressions.push_copy(expression)?;
            }
            catalog.ensure_target_select_with_control(
                TargetSelectPrivilegeRequest {
                    table: &statement.table,
                    privilege_subject: statement.target_privilege_subject.as_ref(),
                    target_qualifier: &statement.target_qualifier,
                    returning_aliases: &statement.returning_aliases,
                    expressions: &expressions,
                    subqueries: &statement.subqueries,
                    required_columns: &[],
                },
                control,
            )?;
            Ok(expressions.finish()?)
        },
    )
}

pub fn insert_target_columns_with_control(
    catalog: &dyn MutationPrivilegeCatalog,
    statement: &InsertPlan,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<String>>, SQLError> {
    insert_supplied_columns_with_control(
        statement,
        |read| catalog.visit_bound_table_column_names_with_control(&statement.table, control, read),
        control,
    )
}

pub fn insert_supplied_columns_with_control(
    statement: &InsertPlan,
    target_columns: impl FnOnce(&mut dyn FnMut(&str) -> Result<(), SQLError>) -> Result<(), SQLError>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<String>>, SQLError> {
    let mut names = ProductionVec::new(*control);
    insert_columns(statement, target_columns, &mut names, control)?;
    Ok(names.finish()?)
}

/// Retain INSERT and conflict-assignment names in one destination through constraint diagnostics.
pub fn insert_constraint_columns_with_control(
    catalog: &dyn MutationPrivilegeCatalog,
    statement: &InsertPlan,
    conflict_update_columns: Option<&[String]>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<String>>, SQLError> {
    let mut names = ProductionVec::new(*control);
    insert_columns(
        statement,
        |read| catalog.visit_bound_table_column_names_with_control(&statement.table, control, read),
        &mut names,
        control,
    )?;
    for name in conflict_update_columns.into_iter().flatten() {
        names.push_produced(control.copy_text(name)?)?;
    }
    Ok(names.finish()?)
}

fn insert_columns(
    statement: &InsertPlan,
    target_columns: impl FnOnce(&mut dyn FnMut(&str) -> Result<(), SQLError>) -> Result<(), SQLError>,
    names: &mut ProductionVec<'_, String>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    if !statement.columns.is_empty() {
        for target in &statement.columns {
            names.push_produced(control.copy_text(&target.column)?)?;
        }
        return Ok(());
    }
    let supplied = match statement.source.as_deref() {
        Some(source) => {
            crate::semantics::projection::query_plan_output_width_with_control(source, control)?
        }
        None => statement.rows.first().map(Vec::len),
    };
    if supplied == Some(0) {
        return Ok(());
    }
    target_columns(&mut |name| {
        control.check()?;
        if supplied.is_none_or(|width| names.len() < width) {
            names.push_produced(control.copy_text(name)?)?;
        }
        Ok(())
    })?;
    Ok(())
}

pub fn assignment_column_names_with_control(
    assignments: &[AssignmentPlan],
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<String>>, SQLError> {
    let mut names = ProductionVec::new(*control);
    for column in assignments
        .iter()
        .flat_map(|assignment| assignment.target.column_names())
    {
        names.push_produced(control.copy_text(column)?)?;
    }
    Ok(names.finish()?)
}

pub fn conflict_update_columns_with_control(
    statement: &InsertPlan,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Vec<String>>>, SQLError> {
    control.check()?;
    match statement
        .on_conflict
        .as_ref()
        .map(|conflict| &conflict.action)
    {
        Some(ConflictActionPlan::Update { assignments, .. }) => {
            assignment_column_names_with_control(assignments, control).map(Some)
        }
        _ => Ok(None),
    }
}
