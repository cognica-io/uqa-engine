//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Privilege checks at each view boundary before DML rewrite or trigger dispatch.

use super::privileges::TargetSelectPrivilegeRequest;
use crate::{
    catalog::{roles::RoleReference, security::table::TableAclPrivilege, stored_view::StoredView},
    plan::{DeletePlan, InsertPlan, MergePlan, UpdatePlan},
    SQLError,
};
use uqa_core::memory::{Produced, ProductionControl};

mod production;
pub use production::{
    ensure_delete_with_control, ensure_insert_with_control, ensure_merge_with_control,
    ensure_update_with_control, merge_privilege_expressions_with_control,
    next_layer_privilege_subject_with_control,
};

/// The next view layer uses its owner or, for `security_invoker`, the statement caller independently of an enclosing view owner.
pub fn next_layer_privilege_subject(
    services: &dyn ViewPrivilegeCatalog,
    name: &str,
) -> Result<RoleReference, SQLError> {
    Ok(next_layer_privilege_subject_with_control(
        services,
        name,
        &ProductionControl::uncontrolled(),
    )?
    .into_uncontrolled()
    .expect("ordinary view privilege subject"))
}

pub fn ensure_insert(
    services: &dyn ViewPrivilegeCatalog,
    statement: &InsertPlan,
) -> Result<RoleReference, SQLError> {
    Ok(
        ensure_insert_with_control(services, statement, &ProductionControl::uncontrolled())?
            .into_uncontrolled()
            .expect("ordinary view privilege subject"),
    )
}

pub fn ensure_update(
    services: &dyn ViewPrivilegeCatalog,
    statement: &UpdatePlan,
) -> Result<RoleReference, SQLError> {
    Ok(
        ensure_update_with_control(services, statement, &ProductionControl::uncontrolled())?
            .into_uncontrolled()
            .expect("ordinary view privilege subject"),
    )
}

pub fn ensure_delete(
    services: &dyn ViewPrivilegeCatalog,
    statement: &DeletePlan,
) -> Result<RoleReference, SQLError> {
    Ok(
        ensure_delete_with_control(services, statement, &ProductionControl::uncontrolled())?
            .into_uncontrolled()
            .expect("ordinary view privilege subject"),
    )
}

pub fn ensure_merge(
    services: &dyn ViewPrivilegeCatalog,
    statement: &MergePlan,
) -> Result<RoleReference, SQLError> {
    Ok(
        ensure_merge_with_control(services, statement, &ProductionControl::uncontrolled())?
            .into_uncontrolled()
            .expect("ordinary view privilege subject"),
    )
}

pub fn merge_privilege_expressions(statement: &MergePlan) -> Vec<&crate::ScalarExpr> {
    merge_privilege_expressions_with_control(statement, &ProductionControl::uncontrolled())
        .expect("ordinary merge privilege expressions")
        .into_uncontrolled()
        .expect("ordinary merge privilege expressions")
}

/// Authorization access to one loaded view and the current statement's SELECT privileges.
pub trait ViewPrivilegeCatalog {
    fn ensure_target_select_with_control(
        &self,
        request: TargetSelectPrivilegeRequest<'_, '_>,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        self.ensure_target_select(request)?;
        Ok(control.check()?)
    }
    fn ensure_view_privilege_for_with_control(
        &self,
        name: &str,
        view: &StoredView,
        subject: &RoleReference,
        privilege: TableAclPrivilege,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        self.ensure_view_privilege_for(name, view, subject, privilege)?;
        Ok(control.check()?)
    }
    fn ensure_view_column_privilege_for_with_control(
        &self,
        name: &str,
        view: &StoredView,
        column: &str,
        subject: &RoleReference,
        privilege: TableAclPrivilege,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        self.ensure_view_column_privilege_for(name, view, column, subject, privilege)?;
        Ok(control.check()?)
    }
    fn ensure_any_view_column_privilege_for_with_control(
        &self,
        name: &str,
        view: &StoredView,
        subject: &RoleReference,
        privilege: TableAclPrivilege,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        self.ensure_any_view_column_privilege_for(name, view, subject, privilege)?;
        Ok(control.check()?)
    }

    /// External authorization catalogs retain their existing callback boundary; native adapters lend their immutable generation.
    fn with_view_definition_with_control(
        &self,
        name: &str,
        control: &ProductionControl<'_>,
        read: &mut dyn FnMut(Option<&StoredView>) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        control.check()?;
        let view = self.view_definition(name)?;
        read(view.as_ref())?;
        Ok(control.check()?)
    }
    fn ensure_query_select_with_control(
        &self,
        query: &crate::plan::QueryPlan,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        self.ensure_query_select(query)?;
        Ok(control.check()?)
    }
    fn current_role_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<RoleReference>, SQLError> {
        control.check()?;
        self.current_role().clone_with_control(control)
    }
    fn bound_role_with_control(
        &self,
        identity: crate::catalog::roles::RoleIdentity,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<RoleReference>, SQLError> {
        control.check()?;
        self.bound_role(identity)?.clone_with_control(control)
    }
    fn ensure_query_select(&self, query: &crate::plan::QueryPlan) -> Result<(), SQLError>;
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

#[cfg(test)]
mod tests;
