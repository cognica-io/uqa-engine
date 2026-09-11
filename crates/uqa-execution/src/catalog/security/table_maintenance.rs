//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Select maintenance targets and release catalog guards before reporting skipped tables.
use super::table_authorization::TableAuthorizationContext;
use crate::catalog::notices::CatalogNotices;
use uqa_sql::{
    catalog::security::table::{role_has_privilege, TableAclPrivilege, TablePrivilegeCheck},
    SQLError,
};
pub struct TableMaintenanceContext<'a> {
    pub authorization: TableAuthorizationContext<'a>,
    pub notices: &'a dyn CatalogNotices,
}
impl TableMaintenanceContext<'_> {
    pub fn maintenance_table_names(&self, operation: &str) -> Result<Vec<String>, SQLError> {
        self.authorization
            .registry
            .refresh_tables()
            .map_err(|error| SQLError::Internal(format!("load tables for {operation}: {error}")))?;
        let tables = self
            .authorization
            .registry
            .tables()
            .security_entries()
            .collect::<Vec<_>>();
        let current_user = self.authorization.names.current_user_name();
        let roles = self.authorization.roles.role_definitions();
        let memberships = self.authorization.roles.role_memberships();
        let mut permitted = Vec::new();
        let mut denied = Vec::new();
        for (relation, security) in tables {
            if role_has_privilege(
                &security,
                &current_user,
                TablePrivilegeCheck {
                    privilege: TableAclPrivilege::Maintain,
                    grant_option: false,
                },
                &roles,
                &memberships,
            ) {
                permitted.push(relation.qualified_name());
            } else {
                denied.push(relation.name);
            }
        }
        drop(memberships);
        drop(roles);
        for name in denied {
            self.notices.notice(
                "WARNING",
                &format!("permission denied to {operation} \"{name}\", skipping it"),
            );
        }
        Ok(permitted)
    }
}
