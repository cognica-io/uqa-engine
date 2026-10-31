//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered table and view MERGE requirements share admitted borrowed keys.

use super::{with_subject, MutationPrivilegeCatalog};
use crate::{
    catalog::security::table::TableAclPrivilege,
    plan::{MergePlan, MergeWhenPlan},
    SQLError,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionMap, ProductionVec};

pub(in crate::semantics) struct MergePrivileges<'c, 'a> {
    pub requires_delete: bool,
    pub requires_any_insert: bool,
    pub privileges: ProductionMap<'c, (TableAclPrivilege, &'a str), ()>,
}

impl<'c, 'a> MergePrivileges<'c, 'a> {
    pub fn new(control: &ProductionControl<'c>) -> Self {
        Self {
            requires_delete: false,
            requires_any_insert: false,
            privileges: ProductionMap::new(*control),
        }
    }

    /// Record one clause after the caller's original target validation, if that boundary has one.
    pub fn record(
        &mut self,
        clause: &'a MergeWhenPlan,
        available: &'a [String],
    ) -> Result<(), SQLError> {
        match clause {
            MergeWhenPlan::InsertNotMatched {
                columns, values, ..
            } => {
                if columns.is_empty() && values.is_empty() {
                    self.requires_any_insert = true;
                } else if columns.is_empty() {
                    for column in available.iter().take(values.len()) {
                        self.privileges
                            .insert_copy((TableAclPrivilege::Insert, column.as_str()), ())?;
                    }
                } else {
                    for target in columns {
                        self.privileges
                            .insert_copy((TableAclPrivilege::Insert, target.column.as_str()), ())?;
                    }
                }
            }
            MergeWhenPlan::UpdateMatched { assignments, .. }
            | MergeWhenPlan::UpdateNotMatchedBySource { assignments, .. } => {
                for column in assignments
                    .iter()
                    .flat_map(|assignment| assignment.target.column_names())
                {
                    self.privileges
                        .insert_copy((TableAclPrivilege::Update, column), ())?;
                }
            }
            MergeWhenPlan::DeleteMatched { .. }
            | MergeWhenPlan::DeleteNotMatchedBySource { .. } => self.requires_delete = true,
            _ => {}
        }
        Ok(())
    }
}

pub fn ensure_merge_mutation_privileges_with_control(
    catalog: &dyn MutationPrivilegeCatalog,
    statement: &MergePlan,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let available = implicit_target_columns(catalog, statement, control)?;
    with_subject(
        catalog,
        statement.target_privilege_subject.as_ref(),
        control,
        |subject| {
            let mut selected = MergePrivileges::new(control);
            for clause in &statement.when_clauses {
                control.check()?;
                selected.record(clause, &available)?;
            }
            if selected.requires_delete {
                catalog.ensure_table_privilege_for_with_control(
                    &statement.target,
                    subject,
                    TableAclPrivilege::Delete,
                    control,
                )?;
            }
            if selected.requires_any_insert {
                catalog.ensure_any_column_privilege_for_with_control(
                    &statement.target,
                    subject,
                    TableAclPrivilege::Insert,
                    control,
                )?;
            }
            for ((privilege, column), ()) in &selected.privileges {
                catalog.ensure_column_privilege_for_with_control(
                    &statement.target,
                    column,
                    subject,
                    *privilege,
                    control,
                )?;
            }
            Ok(())
        },
    )
}

fn implicit_target_columns(
    catalog: &dyn MutationPrivilegeCatalog,
    statement: &MergePlan,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<String>>, SQLError> {
    let mut width = 0;
    for clause in &statement.when_clauses {
        control.check()?;
        if let MergeWhenPlan::InsertNotMatched {
            columns, values, ..
        } = clause
        {
            if columns.is_empty() {
                width = width.max(values.len());
            }
        }
    }
    let mut available = ProductionVec::new(*control);
    // Even explicit targets observe the original table lookup before selecting their subject.
    catalog.visit_bound_table_column_names_with_control(
        &statement.target,
        control,
        &mut |name| {
            control.check()?;
            if available.len() < width {
                available.push_produced(control.copy_text(name)?)?;
            }
            Ok(())
        },
    )?;
    Ok(available.finish()?)
}

pub fn merge_target_columns_with_control(
    catalog: &dyn MutationPrivilegeCatalog,
    statement: &MergePlan,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<String>>, SQLError> {
    let available = implicit_target_columns(catalog, statement, control)?;
    merge_supplied_columns_with_control(statement, &available, control)
}

pub fn merge_supplied_columns_with_control(
    statement: &MergePlan,
    available: &[String],
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<String>>, SQLError> {
    let mut supplied = ProductionVec::new(*control);
    for clause in &statement.when_clauses {
        control.check()?;
        match clause {
            MergeWhenPlan::InsertNotMatched {
                columns, values, ..
            } => {
                if columns.is_empty() {
                    for name in available.iter().take(values.len()) {
                        supplied.push_produced(control.copy_text(name)?)?;
                    }
                } else {
                    for target in columns {
                        supplied.push_produced(control.copy_text(&target.column)?)?;
                    }
                }
            }
            MergeWhenPlan::UpdateMatched { assignments, .. }
            | MergeWhenPlan::UpdateNotMatchedBySource { assignments, .. } => {
                for name in assignments
                    .iter()
                    .flat_map(|assignment| assignment.target.column_names())
                {
                    supplied.push_produced(control.copy_text(name)?)?;
                }
            }
            _ => {}
        }
    }
    Ok(supplied.finish()?)
}

pub fn merge_update_columns_with_control(
    statement: &MergePlan,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<String>>, SQLError> {
    let mut selected = ProductionMap::new(*control);
    for clause in &statement.when_clauses {
        control.check()?;
        if let MergeWhenPlan::UpdateMatched { assignments, .. }
        | MergeWhenPlan::UpdateNotMatchedBySource { assignments, .. } = clause
        {
            for column in assignments
                .iter()
                .flat_map(|assignment| assignment.target.column_names())
            {
                selected.insert_copy(column, ())?;
            }
        }
    }
    let mut names = ProductionVec::new(*control);
    for (name, ()) in &selected {
        names.push_produced(control.copy_text(name)?)?;
    }
    Ok(names.finish()?)
}
