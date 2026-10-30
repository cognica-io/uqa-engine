//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mutation target privilege analysis.

use crate::{
    catalog::{roles::RoleReference, security::table::TableAclPrivilege},
    plan::UpdatePlan,
    semantics::view_privileges::ViewPrivilegeCatalog,
    SQLError, ScalarExpr,
};
use uqa_core::memory::ProductionControl;

pub(super) mod merge;
mod production;
pub use production::{
    assignment_column_names_with_control, conflict_update_columns_with_control,
    ensure_delete_target_privileges_with_control, ensure_insert_target_privileges_with_control,
    ensure_update_target_privileges_with_control, insert_constraint_columns_with_control,
    insert_supplied_columns_with_control, insert_target_columns_with_control,
};
pub trait MutationPrivilegeCatalog: ViewPrivilegeCatalog {
    fn bound_table_column_names(&self, table: &str) -> Result<Vec<String>, SQLError>;
    fn ensure_table_privilege_for(
        &self,
        table: &str,
        subject: &RoleReference,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError>;
    fn ensure_column_privilege_for(
        &self,
        table: &str,
        column: &str,
        subject: &RoleReference,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError>;
    fn ensure_any_column_privilege_for(
        &self,
        table: &str,
        subject: &RoleReference,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError>;
    /// Native callers lend declarations and copy only the selected prefix before releasing the guard.
    fn visit_bound_table_column_names_with_control(
        &self,
        table: &str,
        control: &ProductionControl<'_>,
        read: &mut dyn FnMut(&str) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        control.check()?;
        for name in self.bound_table_column_names(table)? {
            control.check()?;
            read(&name)?;
        }
        Ok(control.check()?)
    }
    fn ensure_table_privilege_for_with_control(
        &self,
        table: &str,
        subject: &RoleReference,
        privilege: TableAclPrivilege,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        self.ensure_table_privilege_for(table, subject, privilege)?;
        Ok(control.check()?)
    }
    fn ensure_column_privilege_for_with_control(
        &self,
        table: &str,
        column: &str,
        subject: &RoleReference,
        privilege: TableAclPrivilege,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        self.ensure_column_privilege_for(table, column, subject, privilege)?;
        Ok(control.check()?)
    }
    fn ensure_any_column_privilege_for_with_control(
        &self,
        table: &str,
        subject: &RoleReference,
        privilege: TableAclPrivilege,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        self.ensure_any_column_privilege_for(table, subject, privilege)?;
        Ok(control.check()?)
    }
}

pub(super) fn with_subject<T>(
    catalog: &dyn MutationPrivilegeCatalog,
    selected: Option<&RoleReference>,
    control: &ProductionControl<'_>,
    authorize: impl FnOnce(&RoleReference) -> Result<T, SQLError>,
) -> Result<T, SQLError> {
    control.check()?;
    if let Some(subject) = selected {
        authorize(subject)
    } else {
        let subject = catalog.current_role_with_control(control)?;
        authorize(&subject)
    }
}

pub fn ensure_update_target_privileges<'a>(
    catalog: &dyn MutationPrivilegeCatalog,
    statement: &'a UpdatePlan,
) -> Result<Vec<&'a ScalarExpr>, SQLError> {
    Ok(ensure_update_target_privileges_with_control(
        catalog,
        statement,
        &ProductionControl::uncontrolled(),
    )?
    .into_uncontrolled()
    .expect("ordinary UPDATE privilege references"))
}

/// Analyze INSERT and ON CONFLICT privileges before any trigger or row mutation.
pub fn ensure_insert_target_privileges(
    catalog: &dyn MutationPrivilegeCatalog,
    statement: &crate::plan::InsertPlan,
    conflict_update_columns: Option<&[String]>,
) -> Result<(), SQLError> {
    ensure_insert_target_privileges_with_control(
        catalog,
        statement,
        conflict_update_columns,
        &ProductionControl::uncontrolled(),
    )
}

/// The columns an INSERT supplies, matching `PostgreSQL` `insertedCols`: explicit targets or the supplied leading columns; DEFAULT VALUES supplies none.
pub fn insert_target_columns(
    catalog: &dyn MutationPrivilegeCatalog,
    statement: &crate::plan::InsertPlan,
) -> Result<Vec<String>, SQLError> {
    Ok(
        insert_target_columns_with_control(catalog, statement, &ProductionControl::uncontrolled())?
            .into_uncontrolled()
            .expect("ordinary INSERT target names"),
    )
}

/// Observe supplied INSERT columns through the target's original ordered declaration callback.
pub fn insert_supplied_columns(
    statement: &crate::plan::InsertPlan,
    target_columns: impl FnOnce() -> Result<Vec<String>, SQLError>,
) -> Result<Vec<String>, SQLError> {
    Ok(insert_supplied_columns_with_control(
        statement,
        |read| {
            for name in target_columns()? {
                read(&name)?;
            }
            Ok(())
        },
        &ProductionControl::uncontrolled(),
    )?
    .into_uncontrolled()
    .expect("ordinary INSERT supplied names"))
}

/// Authorize a DELETE target and the columns read by its predicate and RETURNING.
pub fn ensure_delete_target_privileges<'a>(
    catalog: &dyn MutationPrivilegeCatalog,
    statement: &'a crate::plan::DeletePlan,
) -> Result<Vec<&'a ScalarExpr>, SQLError> {
    Ok(ensure_delete_target_privileges_with_control(
        catalog,
        statement,
        &ProductionControl::uncontrolled(),
    )?
    .into_uncontrolled()
    .expect("ordinary DELETE privilege references"))
}

/// Fill missing DML privilege subjects from the surrounding WITH definition.
pub fn inherit_command_privilege_subject(
    command: &mut crate::plan::CommandPlan,
    subject: RoleReference,
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

#[cfg(test)]
mod tests;
