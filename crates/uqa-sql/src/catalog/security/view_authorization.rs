//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! View and public-column access rules using the current authorization catalogs.
use crate::{
    catalog::{
        roles::{guards::RoleCatalogGuards, identity::RoleSubject},
        security::table::{selection::RelationPrivileges, TableAclPrivilege},
        stored_view::StoredView,
        view::StoredViewKind,
    },
    SQLError,
};
use uqa_core::{memory::ProductionControl, RelationIdentity};

pub struct ViewAuthorizationContext<'a> {
    pub roles: &'a dyn RoleCatalogGuards,
}
impl ViewAuthorizationContext<'_> {
    pub fn ensure_view_privilege_for(
        &self,
        name: &str,
        view: &StoredView,
        subject: &(impl RoleSubject + ?Sized),
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        self.ensure_view_privilege_for_with_control(
            name,
            view,
            subject,
            privilege,
            &ProductionControl::uncontrolled(),
        )
    }
    pub fn ensure_view_column_privilege_for(
        &self,
        name: &str,
        view: &StoredView,
        column: &str,
        subject: &(impl RoleSubject + ?Sized),
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        self.ensure_view_column_privilege_for_with_control(
            name,
            view,
            column,
            subject,
            privilege,
            &ProductionControl::uncontrolled(),
        )
    }
    pub fn ensure_any_view_column_privilege_for(
        &self,
        name: &str,
        view: &StoredView,
        subject: &(impl RoleSubject + ?Sized),
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        self.ensure_any_view_column_privilege_for_with_control(
            name,
            view,
            subject,
            privilege,
            &ProductionControl::uncontrolled(),
        )
    }
    pub fn ensure_view_privilege_for_with_control(
        &self,
        name: &str,
        view: &StoredView,
        subject: &(impl RoleSubject + ?Sized),
        privilege: TableAclPrivilege,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        self.require(name, view, None, subject, privilege, control)
    }
    pub fn ensure_view_column_privilege_for_with_control(
        &self,
        name: &str,
        view: &StoredView,
        column: &str,
        subject: &(impl RoleSubject + ?Sized),
        privilege: TableAclPrivilege,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        self.require(name, view, Some(column), subject, privilege, control)
    }
    fn require(
        &self,
        name: &str,
        view: &StoredView,
        column: Option<&str>,
        subject: &(impl RoleSubject + ?Sized),
        privilege: TableAclPrivilege,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        let relation = RelationIdentity::try_borrowed_reference(name)
            .map_err(|error| SQLError::Internal(error.to_string()))?;
        self.roles
            .with_role_catalogs_with_control(control, &mut |roles, memberships| {
                let access = RelationPrivileges {
                    subject,
                    roles,
                    memberships,
                    control: *control,
                };
                if access.relation_for(&view.security, column, privilege)? {
                    return Ok(());
                }
                let kind = match view.kind {
                    StoredViewKind::View => "view",
                    StoredViewKind::Materialized => "materialized view",
                };
                Err(SQLError::Routine {
                    sqlstate: "42501".into(),
                    message: control
                        .format(format_args!(
                            "permission denied for {kind} {}",
                            relation.name
                        ))?
                        .into_parts()
                        .0,
                })
            })
    }

    pub fn ensure_any_view_column_privilege_for_with_control(
        &self,
        name: &str,
        view: &StoredView,
        subject: &(impl RoleSubject + ?Sized),
        privilege: TableAclPrivilege,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        let security = &view.security;
        let mut allowed = false;
        self.roles
            .with_role_catalogs_with_control(control, &mut |roles, memberships| {
                let access = RelationPrivileges {
                    subject,
                    roles,
                    memberships,
                    control: *control,
                };
                access.validate(
                    security.role_owner,
                    security.acl.as_deref(),
                    security.column_acls.values().map(Vec::as_slice),
                )?;
                let columns = view.output_columns.as_deref().ok_or_else(|| {
                    match control.format(format_args!(
                        "loaded view `{name}` has no durable public column metadata"
                    )) {
                        Ok(message) => SQLError::Internal(message.into_parts().0),
                        Err(error) => error.into(),
                    }
                })?;
                if access.allowed(
                    security.role_owner,
                    security.acl.as_deref(),
                    None,
                    privilege,
                )? {
                    allowed = true;
                    return Ok(());
                }
                for column in columns {
                    control.check()?;
                    if access.allowed(
                        security.role_owner,
                        security.acl.as_deref(),
                        security.column_acls.get(column).map(Vec::as_slice),
                        privilege,
                    )? {
                        allowed = true;
                        break;
                    }
                }
                Ok(())
            })?;
        if allowed {
            return Ok(());
        }
        self.ensure_view_privilege_for_with_control(name, view, subject, privilege, control)
    }
}

#[cfg(test)]
mod tests;
