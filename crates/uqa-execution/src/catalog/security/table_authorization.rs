//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Authorize access through retained table generations and read-only catalog guards.
use super::table_inquiry::{TablePrivilegeRegistry, TablePrivilegeState};
use std::sync::Arc;
use uqa_core::RelationIdentity;
use uqa_sql::{
    catalog::{
        roles::{guards::RoleCatalogGuards, role_inherits, RoleReferenceNames},
        security::{
            columns::role_has_column_privilege as column_privilege_check,
            ownership::RelationOwnerSchemas,
            table::{role_has_privilege, TableAclPrivilege, TablePrivilegeCheck},
        },
    },
    SQLError,
};
pub struct TableAuthorizationContext<'a> {
    pub names: &'a dyn RoleReferenceNames,
    pub roles: &'a dyn RoleCatalogGuards,
    pub registry: &'a dyn TablePrivilegeRegistry,
    pub schemas: &'a dyn RelationOwnerSchemas,
}
impl TableAuthorizationContext<'_> {
    pub fn ensure_table_privilege(
        &self,
        name: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        let current_user = self.names.current_user_name();
        self.ensure_table_privilege_for(name, &current_user, privilege)
    }
    pub fn ensure_table_privilege_for(
        &self,
        name: &str,
        subject: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        let (relation, table) = self.bound_table_for_security(name)?;
        let security = table.security();
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
            message: format!("permission denied for table {}", relation.name),
        })
    }
    pub fn bound_table_column_names(&self, name: &str) -> Result<Vec<String>, SQLError> {
        let (_, table) = self.bound_table_for_security(name)?;
        let columns = table
            .columns()
            .iter()
            .map(|column| column.name.clone())
            .collect();
        Ok(columns)
    }
    pub fn ensure_column_privilege(
        &self,
        name: &str,
        column: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        let current_user = self.names.current_user_name();
        self.ensure_column_privilege_for(name, column, &current_user, privilege)
    }
    pub fn ensure_column_privilege_for(
        &self,
        name: &str,
        column: &str,
        subject: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        let (relation, table) = self.bound_table_for_security(name)?;
        let security = table.security();
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
            message: format!("permission denied for table {}", relation.name),
        })
    }
    pub fn ensure_any_column_privilege(
        &self,
        name: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        let current_user = self.names.current_user_name();
        self.ensure_any_column_privilege_for(name, &current_user, privilege)
    }
    pub fn ensure_any_column_privilege_for(
        &self,
        name: &str,
        subject: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        let (relation, table) = self.bound_table_for_security(name)?;
        let security = table.security();
        let roles = self.roles.role_definitions();
        let memberships = self.roles.role_memberships();
        let table_check = TablePrivilegeCheck {
            privilege,
            grant_option: false,
        };
        if role_has_privilege(&security, subject, table_check, &roles, &memberships)
            || table.columns().iter().any(|column| {
                column_privilege_check(
                    &security,
                    &column.name,
                    subject,
                    table_check,
                    &roles,
                    &memberships,
                )
            })
        {
            return Ok(());
        }
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!("permission denied for table {}", relation.name),
        })
    }
    pub fn ensure_table_owner(&self, name: &str) -> Result<String, SQLError> {
        let (relation, table) = self.bound_table_for_security(name)?;
        let owner = table.role_owner();
        if self.current_user_has_role_privileges(&owner) {
            return Ok(owner);
        }
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!("must be owner of table {}", relation.name),
        })
    }
    pub fn ensure_table_drop_authority(&self, name: &str) -> Result<(), SQLError> {
        let (relation, table) = self.bound_table_for_security(name)?;
        let table_owner = table.role_owner();
        if self.current_user_has_role_privileges(&table_owner) {
            return Ok(());
        }
        if self
            .schemas
            .schema_security(&relation.schema)
            .is_some_and(|security| self.current_user_has_role_privileges(&security.role_owner))
        {
            return Ok(());
        }
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!("must be owner of table {}", relation.name),
        })
    }
    pub fn bound_table_for_security(
        &self,
        name: &str,
    ) -> Result<(RelationIdentity, Arc<dyn TablePrivilegeState>), SQLError> {
        let relation = RelationIdentity::from_legacy_name(name)
            .map_err(|error| SQLError::Internal(format!("resolve table `{name}`: {error}")))?;
        let table = self
            .registry
            .tables()
            .retained(&relation)
            .ok_or_else(|| SQLError::Internal(format!("table `{name}` disappeared")))?;
        Ok((relation, table))
    }
    fn current_user_has_role_privileges(&self, target: &str) -> bool {
        let current = self.names.current_user_name();
        let roles = self.roles.role_definitions();
        let memberships = self.roles.role_memberships();
        role_inherits(&roles, &memberships, &current, target)
    }
}
