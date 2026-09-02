//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Role-aware visibility and ordinary-table privilege projections.

use super::super::{CatalogReadView, CatalogTableSnapshot};

impl CatalogReadView {
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
}
