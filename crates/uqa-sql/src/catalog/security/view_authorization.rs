//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! View and public-column access rules using the current authorization catalogs.
use crate::{
    catalog::{
        roles::guards::RoleCatalogGuards,
        security::{
            columns::role_has_column_privilege as column_privilege_check,
            table::{role_has_privilege, TableAclPrivilege, TablePrivilegeCheck},
        },
        stored_view::StoredView,
        view::StoredViewKind,
    },
    SQLError,
};
use uqa_core::RelationIdentity;
pub struct ViewAuthorizationContext<'a> {
    pub roles: &'a dyn RoleCatalogGuards,
}
impl ViewAuthorizationContext<'_> {
    pub fn ensure_view_privilege_for(
        &self,
        name: &str,
        view: &StoredView,
        subject: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        let relation = RelationIdentity::from_legacy_name(name).map_err(SQLError::Internal)?;
        let security = view.security();
        let roles = self.roles.role_definitions();
        let memberships = self.roles.role_memberships();
        if role_has_privilege(
            &security,
            subject,
            TablePrivilegeCheck {
                privilege,
                grant_option: false,
            },
            &roles,
            &memberships,
        ) {
            return Ok(());
        }
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!(
                "permission denied for {} {}",
                match view.kind {
                    StoredViewKind::View => "view",
                    StoredViewKind::Materialized => "materialized view",
                },
                relation.name
            ),
        })
    }
    pub fn ensure_view_column_privilege_for(
        &self,
        name: &str,
        view: &StoredView,
        column: &str,
        subject: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        let relation = RelationIdentity::from_legacy_name(name).map_err(SQLError::Internal)?;
        let security = view.security();
        let roles = self.roles.role_definitions();
        let memberships = self.roles.role_memberships();
        if column_privilege_check(
            &security,
            column,
            subject,
            TablePrivilegeCheck {
                privilege,
                grant_option: false,
            },
            &roles,
            &memberships,
        ) {
            return Ok(());
        }
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!(
                "permission denied for {} {}",
                match view.kind {
                    StoredViewKind::View => "view",
                    StoredViewKind::Materialized => "materialized view",
                },
                relation.name
            ),
        })
    }
    pub fn ensure_any_view_column_privilege_for(
        &self,
        name: &str,
        view: &StoredView,
        subject: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        let security = view.security();
        let roles = self.roles.role_definitions();
        let memberships = self.roles.role_memberships();
        let check = TablePrivilegeCheck {
            privilege,
            grant_option: false,
        };
        let columns = view.output_columns.as_deref().ok_or_else(|| {
            SQLError::Internal(format!(
                "loaded view `{name}` has no durable public column metadata"
            ))
        })?;
        if role_has_privilege(&security, subject, check, &roles, &memberships)
            || columns.iter().any(|column| {
                column_privilege_check(&security, column, subject, check, &roles, &memberships)
            })
        {
            return Ok(());
        }
        drop(memberships);
        drop(roles);
        self.ensure_view_privilege_for(name, view, subject, privilege)
    }
}

#[cfg(test)]
mod tests;
