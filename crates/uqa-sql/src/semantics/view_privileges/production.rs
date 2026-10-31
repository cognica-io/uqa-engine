//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! View authorization borrows one immutable definition and owns only selected subjects and request scratch.

use super::{TableAclPrivilege, TargetSelectPrivilegeRequest, ViewPrivilegeCatalog};
use crate::semantics::mutation_privileges::merge::MergePrivileges;
use crate::{
    catalog::{roles::RoleReference, stored_view::StoredView},
    plan::{
        ConflictActionPlan, ConflictPlan, DeletePlan, InsertPlan, MergePlan, MergeWhenPlan,
        UpdatePlan,
    },
    SQLError, ScalarExpr,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

fn with_target<T>(
    services: &dyn ViewPrivilegeCatalog,
    name: &str,
    control: &ProductionControl<'_>,
    mut action: impl FnMut(&StoredView, &[String]) -> Result<T, SQLError>,
) -> Result<T, SQLError> {
    let mut result = None;
    services.with_view_definition_with_control(name, control, &mut |view| {
        let view = view.ok_or_else(|| SQLError::UnknownTable(name.to_owned()))?;
        let columns = view.output_columns.as_deref().ok_or_else(|| {
            SQLError::Internal(format!(
                "loaded view `{name}` has no durable public column metadata"
            ))
        })?;
        result = Some(action(view, columns)?);
        Ok(())
    })?;
    result.ok_or_else(|| {
        SQLError::Internal("view privilege catalog did not supply its definition".into())
    })
}

fn subject(
    services: &dyn ViewPrivilegeCatalog,
    current: Option<&RoleReference>,
    control: &ProductionControl<'_>,
) -> Result<Produced<RoleReference>, SQLError> {
    match current {
        Some(current) => current.clone_with_control(control),
        None => services.current_role_with_control(control),
    }
}

fn next(
    services: &dyn ViewPrivilegeCatalog,
    view: &StoredView,
    control: &ProductionControl<'_>,
) -> Result<Produced<RoleReference>, SQLError> {
    control.check()?;
    if view.security_invoker() {
        services.current_role_with_control(control)
    } else {
        services.bound_role_with_control(view.security.role_owner, control)
    }
}

fn validate<'a>(
    name: &str,
    available: &[String],
    requested: impl IntoIterator<Item = &'a str>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    for column in requested {
        control.check()?;
        if !available.iter().any(|available| available == column) {
            return Err(SQLError::UnknownColumn(format!("{name}.{column}")));
        }
    }
    Ok(())
}

fn expressions<'a>(
    values: impl IntoIterator<Item = &'a ScalarExpr>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<&'a ScalarExpr>>, SQLError> {
    let mut output = ProductionVec::new(*control);
    extend(&mut output, values)?;
    Ok(output.finish()?)
}
fn extend<'a>(
    output: &mut ProductionVec<'_, &'a ScalarExpr>,
    values: impl IntoIterator<Item = &'a ScalarExpr>,
) -> Result<(), SQLError> {
    for expression in values {
        output.push_copy(expression)?;
    }
    Ok(())
}
pub fn next_layer_privilege_subject_with_control(
    services: &dyn ViewPrivilegeCatalog,
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<RoleReference>, SQLError> {
    with_target(services, name, control, |view, _| {
        next(services, view, control)
    })
}

pub fn ensure_insert_with_control(
    services: &dyn ViewPrivilegeCatalog,
    statement: &InsertPlan,
    control: &ProductionControl<'_>,
) -> Result<Produced<RoleReference>, SQLError> {
    with_target(services, &statement.table, control, |view, available| {
        crate::assignment::targets::validate_repeated_targets_with_control(
            statement.columns.iter(),
            true,
            control,
        )?;
        validate(
            &statement.table,
            available,
            statement
                .columns
                .iter()
                .map(|target| target.column.as_str()),
            control,
        )?;
        let subject = subject(
            services,
            statement.target_privilege_subject.as_ref(),
            control,
        )?;
        check_insert_columns(services, statement, view, available, &subject, control)?;
        let mut selected = ProductionVec::new(*control);
        extend(
            &mut selected,
            statement
                .returning
                .iter()
                .map(|projection| &projection.expr),
        )?;
        if let Some(conflict) = &statement.on_conflict {
            extend(&mut selected, conflict.expressions.iter())?;
            extend(&mut selected, conflict.predicate.iter().map(Box::as_ref))?;
        }
        let required = if let Some(ConflictPlan {
            conflict_columns,
            action:
                ConflictActionPlan::Update {
                    assignments,
                    predicate,
                },
            ..
        }) = &statement.on_conflict
        {
            crate::assignment::targets::validate_repeated_targets_with_control(
                assignments
                    .iter()
                    .flat_map(|assignment| assignment.target.targets()),
                false,
                control,
            )?;
            validate(
                &statement.table,
                available,
                assignments
                    .iter()
                    .flat_map(|assignment| assignment.target.column_names()),
                control,
            )?;
            for column in assignments
                .iter()
                .flat_map(|assignment| assignment.target.column_names())
            {
                control.check()?;
                services.ensure_view_column_privilege_for_with_control(
                    &statement.table,
                    view,
                    column,
                    &subject,
                    TableAclPrivilege::Update,
                    control,
                )?;
            }
            extend(
                &mut selected,
                assignments
                    .iter()
                    .flat_map(crate::plan::AssignmentPlan::expressions),
            )?;
            extend(&mut selected, predicate.iter().map(Box::as_ref))?;
            conflict_columns.as_slice()
        } else {
            &[]
        };
        services.ensure_target_select_with_control(
            TargetSelectPrivilegeRequest {
                table: &statement.table,
                privilege_subject: Some(&subject),
                target_qualifier: &statement.target_qualifier,
                returning_aliases: &statement.returning_aliases,
                expressions: &selected,
                subqueries: &statement.subqueries,
                required_columns: required,
            },
            control,
        )?;
        drop(subject);
        next(services, view, control)
    })
}

pub fn ensure_update_with_control(
    services: &dyn ViewPrivilegeCatalog,
    statement: &UpdatePlan,
    control: &ProductionControl<'_>,
) -> Result<Produced<RoleReference>, SQLError> {
    with_target(services, &statement.table, control, |view, available| {
        crate::assignment::targets::validate_repeated_targets_with_control(
            statement
                .assignments
                .iter()
                .flat_map(|assignment| assignment.target.targets()),
            false,
            control,
        )?;
        validate(
            &statement.table,
            available,
            statement
                .assignments
                .iter()
                .flat_map(|assignment| assignment.target.column_names()),
            control,
        )?;
        let subject = subject(
            services,
            statement.target_privilege_subject.as_ref(),
            control,
        )?;
        for column in statement
            .assignments
            .iter()
            .flat_map(|assignment| assignment.target.column_names())
        {
            control.check()?;
            services.ensure_view_column_privilege_for_with_control(
                &statement.table,
                view,
                column,
                &subject,
                TableAclPrivilege::Update,
                control,
            )?;
        }
        let expressions = expressions(
            statement
                .assignments
                .iter()
                .flat_map(crate::plan::AssignmentPlan::expressions)
                .chain(statement.predicate.iter())
                .chain(
                    statement
                        .returning
                        .iter()
                        .map(|projection| &projection.expr),
                ),
            control,
        )?;
        services.ensure_target_select_with_control(
            TargetSelectPrivilegeRequest {
                table: &statement.table,
                privilege_subject: Some(&subject),
                target_qualifier: &statement.target_qualifier,
                returning_aliases: &statement.returning_aliases,
                expressions: &expressions,
                subqueries: &statement.subqueries,
                required_columns: &[],
            },
            control,
        )?;
        drop(subject);
        next(services, view, control)
    })
}

pub fn ensure_delete_with_control(
    services: &dyn ViewPrivilegeCatalog,
    statement: &DeletePlan,
    control: &ProductionControl<'_>,
) -> Result<Produced<RoleReference>, SQLError> {
    with_target(services, &statement.table, control, |view, _| {
        let subject = subject(
            services,
            statement.target_privilege_subject.as_ref(),
            control,
        )?;
        services.ensure_view_privilege_for_with_control(
            &statement.table,
            view,
            &subject,
            TableAclPrivilege::Delete,
            control,
        )?;
        let expressions = expressions(
            statement.predicate.iter().chain(
                statement
                    .returning
                    .iter()
                    .map(|projection| &projection.expr),
            ),
            control,
        )?;
        services.ensure_target_select_with_control(
            TargetSelectPrivilegeRequest {
                table: &statement.table,
                privilege_subject: Some(&subject),
                target_qualifier: &statement.target_qualifier,
                returning_aliases: &statement.returning_aliases,
                expressions: &expressions,
                subqueries: &statement.subqueries,
                required_columns: &[],
            },
            control,
        )?;
        drop(subject);
        next(services, view, control)
    })
}

pub fn ensure_merge_with_control(
    services: &dyn ViewPrivilegeCatalog,
    statement: &MergePlan,
    control: &ProductionControl<'_>,
) -> Result<Produced<RoleReference>, SQLError> {
    with_target(services, &statement.target, control, |view, available| {
        let subject = subject(
            services,
            statement.target_privilege_subject.as_ref(),
            control,
        )?;
        let selected = merge_requirements(statement, available, control)?;
        if selected.requires_delete {
            services.ensure_view_privilege_for_with_control(
                &statement.target,
                view,
                &subject,
                TableAclPrivilege::Delete,
                control,
            )?;
        }
        if selected.requires_any_insert {
            services.ensure_any_view_column_privilege_for_with_control(
                &statement.target,
                view,
                &subject,
                TableAclPrivilege::Insert,
                control,
            )?;
        }
        for ((privilege, column), ()) in &selected.privileges {
            control.check()?;
            services.ensure_view_column_privilege_for_with_control(
                &statement.target,
                view,
                column,
                &subject,
                *privilege,
                control,
            )?;
        }
        drop(selected);
        let expressions = merge_privilege_expressions_with_control(statement, control)?;
        services.ensure_target_select_with_control(
            TargetSelectPrivilegeRequest {
                table: &statement.target,
                privilege_subject: Some(&subject),
                target_qualifier: &statement.target_qualifier,
                returning_aliases: &statement.returning_aliases,
                expressions: &expressions,
                subqueries: &statement.subqueries,
                required_columns: &[],
            },
            control,
        )?;
        drop(subject);
        next(services, view, control)
    })
}

pub fn merge_privilege_expressions_with_control<'a>(
    statement: &'a MergePlan,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<&'a ScalarExpr>>, SQLError> {
    let mut expressions = ProductionVec::new(*control);
    expressions.push_copy(&statement.join_condition)?;
    extend(&mut expressions, statement.target_predicate.iter())?;
    extend(
        &mut expressions,
        statement
            .returning
            .iter()
            .map(|projection| &projection.expr),
    )?;
    for clause in &statement.when_clauses {
        match clause {
            MergeWhenPlan::UpdateMatched {
                condition,
                assignments,
            }
            | MergeWhenPlan::UpdateNotMatchedBySource {
                condition,
                assignments,
            } => {
                extend(&mut expressions, condition.iter())?;
                extend(
                    &mut expressions,
                    assignments
                        .iter()
                        .flat_map(crate::plan::AssignmentPlan::expressions),
                )?;
            }
            MergeWhenPlan::InsertNotMatched {
                condition,
                columns,
                values,
                ..
            } => {
                extend(&mut expressions, condition.iter())?;
                extend(
                    &mut expressions,
                    columns
                        .iter()
                        .flat_map(crate::ast::AssignmentTarget::expressions),
                )?;
                extend(&mut expressions, values)?;
            }
            MergeWhenPlan::DeleteMatched { condition }
            | MergeWhenPlan::DeleteNotMatchedBySource { condition }
            | MergeWhenPlan::NothingMatched { condition }
            | MergeWhenPlan::NothingNotMatched { condition }
            | MergeWhenPlan::NothingNotMatchedBySource { condition } => {
                extend(&mut expressions, condition.iter())?;
            }
        }
    }
    Ok(expressions.finish()?)
}

fn check_insert_columns(
    services: &dyn ViewPrivilegeCatalog,
    statement: &InsertPlan,
    view: &StoredView,
    available: &[String],
    subject: &RoleReference,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let default_values = statement.source.is_none()
        && statement.columns.is_empty()
        && statement.rows.iter().all(Vec::is_empty);
    if default_values {
        services.ensure_any_view_column_privilege_for_with_control(
            &statement.table,
            view,
            subject,
            TableAclPrivilege::Insert,
            control,
        )?;
    } else if statement.columns.is_empty() {
        let supplied = match statement.source.as_deref() {
            Some(source) => {
                crate::semantics::projection::query_plan_output_width_with_control(source, control)?
            }
            None => statement.rows.first().map(Vec::len),
        };
        for column in available.iter().take(supplied.unwrap_or(available.len())) {
            control.check()?;
            services.ensure_view_column_privilege_for_with_control(
                &statement.table,
                view,
                column,
                subject,
                TableAclPrivilege::Insert,
                control,
            )?;
        }
    } else {
        for target in &statement.columns {
            control.check()?;
            services.ensure_view_column_privilege_for_with_control(
                &statement.table,
                view,
                &target.column,
                subject,
                TableAclPrivilege::Insert,
                control,
            )?;
        }
    }
    Ok(())
}

fn merge_requirements<'c, 'a>(
    statement: &'a MergePlan,
    available: &'a [String],
    control: &ProductionControl<'c>,
) -> Result<MergePrivileges<'c, 'a>, SQLError> {
    let mut selected = MergePrivileges::new(control);
    for clause in &statement.when_clauses {
        control.check()?;
        match clause {
            MergeWhenPlan::InsertNotMatched { columns, .. } => {
                crate::assignment::targets::validate_repeated_targets_with_control(
                    columns.iter(),
                    true,
                    control,
                )?;
                validate(
                    &statement.target,
                    available,
                    columns.iter().map(|target| target.column.as_str()),
                    control,
                )?;
            }
            MergeWhenPlan::UpdateMatched { assignments, .. }
            | MergeWhenPlan::UpdateNotMatchedBySource { assignments, .. } => {
                crate::assignment::targets::validate_repeated_targets_with_control(
                    assignments
                        .iter()
                        .flat_map(|assignment| assignment.target.targets()),
                    false,
                    control,
                )?;
                validate(
                    &statement.target,
                    available,
                    assignments
                        .iter()
                        .flat_map(|assignment| assignment.target.column_names()),
                    control,
                )?;
            }
            _ => {}
        }
        selected.record(clause, available)?;
    }
    Ok(selected)
}
