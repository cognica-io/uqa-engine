//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Role-aware visibility and table-shaped relation privilege projections.

use super::super::{CatalogReadView, CatalogTableSnapshot};

impl CatalogReadView {
    fn view_security(view: &crate::StoredView) -> crate::engine_state::TableSecurity {
        view.security()
    }

    pub(crate) fn role_is_enabled_for(&self, member: &str, role: &str) -> bool {
        crate::engine_roles::role_inherits(
            &self.snapshot.durable.roles,
            &self.snapshot.durable.role_memberships,
            member,
            role,
        )
    }

    pub(crate) fn table_is_visible_to(&self, table: &CatalogTableSnapshot, role: &str) -> bool {
        crate::engine_table_security::role_can_view_table(
            &crate::engine_state::TableSecurity {
                role_owner: table.role_owner.clone(),
                acl: table.acl.clone(),
                column_acls: table.column_acls.clone(),
            },
            role,
            &self.snapshot.durable.roles,
            &self.snapshot.durable.role_memberships,
        )
    }

    pub(crate) fn table_has_privilege_to(
        &self,
        table: &CatalogTableSnapshot,
        role: &str,
        privilege: crate::engine_table_security::TableAclPrivilege,
    ) -> bool {
        crate::engine_table_security::role_has_table_privilege(
            &crate::engine_state::TableSecurity {
                role_owner: table.role_owner.clone(),
                acl: table.acl.clone(),
                column_acls: table.column_acls.clone(),
            },
            role,
            privilege,
            &self.snapshot.durable.roles,
            &self.snapshot.durable.role_memberships,
        )
    }

    pub(crate) fn table_column_has_privilege_to(
        &self,
        table: &CatalogTableSnapshot,
        column: &str,
        role: &str,
        privilege: crate::engine_table_security::TableAclPrivilege,
    ) -> bool {
        crate::engine_table_security::role_has_column_privilege(
            &crate::engine_state::TableSecurity {
                role_owner: table.role_owner.clone(),
                acl: table.acl.clone(),
                column_acls: table.column_acls.clone(),
            },
            column,
            role,
            privilege,
            &self.snapshot.durable.roles,
            &self.snapshot.durable.role_memberships,
        )
    }

    pub(crate) fn table_column_is_visible_to(
        &self,
        table: &CatalogTableSnapshot,
        column: &str,
        role: &str,
    ) -> bool {
        crate::engine_table_security::TableAclPrivilege::COLUMN_ALL
            .into_iter()
            .any(|privilege| self.table_column_has_privilege_to(table, column, role, privilege))
    }

    pub(crate) fn view_is_visible_to(&self, view: &crate::StoredView, role: &str) -> bool {
        crate::engine_table_security::role_can_view_table(
            &Self::view_security(view),
            role,
            &self.snapshot.durable.roles,
            &self.snapshot.durable.role_memberships,
        )
    }

    pub(crate) fn view_has_privilege_to(
        &self,
        view: &crate::StoredView,
        role: &str,
        privilege: crate::engine_table_security::TableAclPrivilege,
    ) -> bool {
        crate::engine_table_security::role_has_table_privilege(
            &Self::view_security(view),
            role,
            privilege,
            &self.snapshot.durable.roles,
            &self.snapshot.durable.role_memberships,
        )
    }

    pub(crate) fn view_column_has_privilege_to(
        &self,
        view: &crate::StoredView,
        column: &str,
        role: &str,
        privilege: crate::engine_table_security::TableAclPrivilege,
    ) -> bool {
        crate::engine_table_security::role_has_column_privilege(
            &Self::view_security(view),
            column,
            role,
            privilege,
            &self.snapshot.durable.roles,
            &self.snapshot.durable.role_memberships,
        )
    }

    pub(crate) fn view_column_is_visible_to(
        &self,
        view: &crate::StoredView,
        column: &str,
        role: &str,
    ) -> bool {
        crate::engine_table_security::TableAclPrivilege::COLUMN_ALL
            .into_iter()
            .any(|privilege| self.view_column_has_privilege_to(view, column, role, privilege))
    }

    pub(crate) fn foreign_table_is_visible_to(
        &self,
        name: &str,
        role: &str,
    ) -> Result<bool, uqa_sql::SQLError> {
        Ok(crate::engine_table_security::role_can_view_table(
            self.foreign_table_security(name)?,
            role,
            &self.snapshot.durable.roles,
            &self.snapshot.durable.role_memberships,
        ))
    }

    pub(crate) fn foreign_table_has_privilege_to(
        &self,
        name: &str,
        role: &str,
        privilege: crate::engine_table_security::TableAclPrivilege,
    ) -> Result<bool, uqa_sql::SQLError> {
        Ok(crate::engine_table_security::role_has_table_privilege(
            self.foreign_table_security(name)?,
            role,
            privilege,
            &self.snapshot.durable.roles,
            &self.snapshot.durable.role_memberships,
        ))
    }

    pub(crate) fn foreign_table_column_has_privilege_to(
        &self,
        name: &str,
        column: &str,
        role: &str,
        privilege: crate::engine_table_security::TableAclPrivilege,
    ) -> Result<bool, uqa_sql::SQLError> {
        Ok(crate::engine_table_security::role_has_column_privilege(
            self.foreign_table_security(name)?,
            column,
            role,
            privilege,
            &self.snapshot.durable.roles,
            &self.snapshot.durable.role_memberships,
        ))
    }

    pub(crate) fn foreign_table_column_is_visible_to(
        &self,
        name: &str,
        column: &str,
        role: &str,
    ) -> Result<bool, uqa_sql::SQLError> {
        for privilege in crate::engine_table_security::TableAclPrivilege::COLUMN_ALL {
            if self.foreign_table_column_has_privilege_to(name, column, role, privilege)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
