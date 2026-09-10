//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Role-aware visibility and table-shaped relation privilege projections.

use super::super::{CatalogReadView, CatalogTableSnapshot};

impl CatalogReadView {
    fn view_security(
        view: &crate::catalog::view::StoredView,
    ) -> crate::catalog::security::TableSecurity {
        view.security()
    }

    pub fn role_is_enabled_for(&self, member: &str, role: &str) -> bool {
        uqa_sql::catalog::roles::role_inherits(
            &self.snapshot.definitions.roles,
            &self.snapshot.definitions.role_memberships,
            member,
            role,
        )
    }

    pub fn table_is_visible_to(&self, table: &CatalogTableSnapshot, role: &str) -> bool {
        crate::catalog::security::table::role_can_view_table(
            &table.security,
            role,
            &self.snapshot.definitions.roles,
            &self.snapshot.definitions.role_memberships,
        )
    }

    pub fn table_has_privilege_to(
        &self,
        table: &CatalogTableSnapshot,
        role: &str,
        privilege: crate::catalog::security::table::TableAclPrivilege,
    ) -> bool {
        crate::catalog::security::table::role_has_table_privilege(
            &table.security,
            role,
            privilege,
            &self.snapshot.definitions.roles,
            &self.snapshot.definitions.role_memberships,
        )
    }

    pub fn table_column_has_privilege_to(
        &self,
        table: &CatalogTableSnapshot,
        column: &str,
        role: &str,
        privilege: crate::catalog::security::table::TableAclPrivilege,
    ) -> bool {
        crate::catalog::security::table::role_has_column_privilege(
            &table.security,
            column,
            role,
            privilege,
            &self.snapshot.definitions.roles,
            &self.snapshot.definitions.role_memberships,
        )
    }

    pub fn table_column_is_visible_to(
        &self,
        table: &CatalogTableSnapshot,
        column: &str,
        role: &str,
    ) -> bool {
        crate::catalog::security::table::TableAclPrivilege::COLUMN_ALL
            .into_iter()
            .any(|privilege| self.table_column_has_privilege_to(table, column, role, privilege))
    }

    pub fn view_is_visible_to(&self, view: &crate::catalog::view::StoredView, role: &str) -> bool {
        crate::catalog::security::table::role_can_view_table(
            &Self::view_security(view),
            role,
            &self.snapshot.definitions.roles,
            &self.snapshot.definitions.role_memberships,
        )
    }

    pub fn view_has_privilege_to(
        &self,
        view: &crate::catalog::view::StoredView,
        role: &str,
        privilege: crate::catalog::security::table::TableAclPrivilege,
    ) -> bool {
        crate::catalog::security::table::role_has_table_privilege(
            &Self::view_security(view),
            role,
            privilege,
            &self.snapshot.definitions.roles,
            &self.snapshot.definitions.role_memberships,
        )
    }

    pub fn view_column_has_privilege_to(
        &self,
        view: &crate::catalog::view::StoredView,
        column: &str,
        role: &str,
        privilege: crate::catalog::security::table::TableAclPrivilege,
    ) -> bool {
        crate::catalog::security::table::role_has_column_privilege(
            &Self::view_security(view),
            column,
            role,
            privilege,
            &self.snapshot.definitions.roles,
            &self.snapshot.definitions.role_memberships,
        )
    }

    pub fn view_column_is_visible_to(
        &self,
        view: &crate::catalog::view::StoredView,
        column: &str,
        role: &str,
    ) -> bool {
        crate::catalog::security::table::TableAclPrivilege::COLUMN_ALL
            .into_iter()
            .any(|privilege| self.view_column_has_privilege_to(view, column, role, privilege))
    }

    pub fn foreign_table_is_visible_to(
        &self,
        name: &str,
        role: &str,
    ) -> Result<bool, uqa_sql::SQLError> {
        Ok(crate::catalog::security::table::role_can_view_table(
            self.foreign_table_security(name)?,
            role,
            &self.snapshot.definitions.roles,
            &self.snapshot.definitions.role_memberships,
        ))
    }

    pub fn foreign_table_has_privilege_to(
        &self,
        name: &str,
        role: &str,
        privilege: crate::catalog::security::table::TableAclPrivilege,
    ) -> Result<bool, uqa_sql::SQLError> {
        Ok(crate::catalog::security::table::role_has_table_privilege(
            self.foreign_table_security(name)?,
            role,
            privilege,
            &self.snapshot.definitions.roles,
            &self.snapshot.definitions.role_memberships,
        ))
    }

    pub fn foreign_table_column_has_privilege_to(
        &self,
        name: &str,
        column: &str,
        role: &str,
        privilege: crate::catalog::security::table::TableAclPrivilege,
    ) -> Result<bool, uqa_sql::SQLError> {
        Ok(crate::catalog::security::table::role_has_column_privilege(
            self.foreign_table_security(name)?,
            column,
            role,
            privilege,
            &self.snapshot.definitions.roles,
            &self.snapshot.definitions.role_memberships,
        ))
    }

    pub fn foreign_table_column_is_visible_to(
        &self,
        name: &str,
        column: &str,
        role: &str,
    ) -> Result<bool, uqa_sql::SQLError> {
        for privilege in crate::catalog::security::table::TableAclPrivilege::COLUMN_ALL {
            if self.foreign_table_column_has_privilege_to(name, column, role, privilege)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
