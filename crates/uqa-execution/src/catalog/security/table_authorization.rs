//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Authorize access through retained table generations and read-only catalog guards.
use super::table_inquiry::{TablePrivilegeRegistry, TablePrivilegeState};
use std::sync::Arc;
use uqa_core::RelationIdentity;
use uqa_sql::catalog::roles::identity::RoleSubject;
use uqa_sql::{
    catalog::{
        roles::{guards::RoleCatalogGuards, role_inherits, RoleReferenceNames},
        security::{
            columns::role_has_column_privilege as column_privilege_check,
            ownership::RelationOwnerSchemas,
            table::{role_has_privilege, TableAclPrivilege, TablePrivilegeCheck},
            BoundTableSecurity,
        },
        stored_view::StoredView,
    },
    SQLError,
};
#[derive(Clone, Copy)]
pub struct TableAuthorizationContext<'a> {
    pub names: &'a dyn RoleReferenceNames,
    pub roles: &'a dyn RoleCatalogGuards,
    pub registry: &'a dyn TablePrivilegeRegistry,
    pub schemas: &'a dyn RelationOwnerSchemas,
}
impl TableAuthorizationContext<'_> {
    /// Expression keys require table SELECT; plain keys may use SELECT on every key column. Included columns do not participate in the diagnostic.
    pub fn can_view_index_key(
        &self,
        name: &str,
        keys: &[uqa_sql::ast::IndexKey],
    ) -> Result<bool, SQLError> {
        let (_, table) = self.bound_table_for_security(name)?;
        let bound = table.security();
        let roles = self.roles.role_definitions();
        let memberships = self.roles.role_memberships();
        let security = bound.resolve(&roles).map_err(SQLError::Internal)?;
        let subject = self.names.current_role();
        let check = TablePrivilegeCheck {
            privilege: TableAclPrivilege::Select,
            grant_option: false,
        };
        Ok(
            role_has_privilege(&security, &subject, check, &roles, &memberships)
                || keys.iter().all(|key| {
                    key.column().is_some_and(|column| {
                        column_privilege_check(
                            &security,
                            column,
                            &subject,
                            check,
                            &roles,
                            &memberships,
                        )
                    })
                }),
        )
    }

    /// The columns of a table that the description of one of its rows shows, as `ExecBuildSlotValueDescription` decides: `None` when the role may read the table, otherwise, in table order, each column the role may read or the statement supplies. The role is the current role, or the table's owner for a referential action, which `PostgreSQL` performs as that owner.
    pub fn row_description_columns(
        &self,
        name: &str,
        supplied: &[String],
        as_owner: bool,
    ) -> Result<Option<Vec<String>>, SQLError> {
        let (_, table) = self.bound_table_for_security(name)?;
        let subject: Box<dyn RoleSubject> = if as_owner {
            Box::new(table.role_owner())
        } else {
            Box::new(self.names.current_role())
        };
        self.visible_row_columns(
            &table.security(),
            subject.as_ref(),
            table.column_names(),
            supplied,
        )
    }

    /// The columns of a view that the description of a row its `INSTEAD OF` trigger stores shows, which `ExecBuildSlotValueDescription` decides for the view as it does for a table: `None` when the current role may read the view, otherwise, in view order, each column the role may read or the statement supplies.
    pub fn view_row_description_columns(
        &self,
        view: &StoredView,
        columns: &[String],
        supplied: &[String],
    ) -> Result<Option<Vec<String>>, SQLError> {
        self.visible_row_columns(
            &view.security(),
            &self.names.current_role(),
            columns.to_vec(),
            supplied,
        )
    }

    fn visible_row_columns(
        &self,
        security: &BoundTableSecurity,
        subject: &dyn RoleSubject,
        columns: Vec<String>,
        supplied: &[String],
    ) -> Result<Option<Vec<String>>, SQLError> {
        let roles = self.roles.role_definitions();
        let memberships = self.roles.role_memberships();
        let security = security.resolve(&roles).map_err(SQLError::Internal)?;
        let check = TablePrivilegeCheck {
            privilege: TableAclPrivilege::Select,
            grant_option: false,
        };
        if role_has_privilege(&security, subject, check, &roles, &memberships) {
            return Ok(None);
        }
        Ok(Some(
            columns
                .into_iter()
                .filter(|column| {
                    supplied.contains(column)
                        || column_privilege_check(
                            &security,
                            column,
                            subject,
                            check,
                            &roles,
                            &memberships,
                        )
                })
                .collect(),
        ))
    }

    /// Whether the description of a row's partition key may show its values, as `ExecBuildSlotPartitionKeyDescription` decides: the role may read the table, or each key is a column it may read. The role is the current role, or the table's owner for a referential action.
    pub fn can_view_partition_key(
        &self,
        name: &str,
        keys: &[uqa_sql::ast::Expr],
        as_owner: bool,
    ) -> Result<bool, SQLError> {
        let (_, table) = self.bound_table_for_security(name)?;
        let roles = self.roles.role_definitions();
        let memberships = self.roles.role_memberships();
        let security = table
            .security()
            .resolve(&roles)
            .map_err(SQLError::Internal)?;
        let subject: Box<dyn RoleSubject> = if as_owner {
            Box::new(table.role_owner())
        } else {
            Box::new(self.names.current_role())
        };
        let check = TablePrivilegeCheck {
            privilege: TableAclPrivilege::Select,
            grant_option: false,
        };
        Ok(
            role_has_privilege(&security, subject.as_ref(), check, &roles, &memberships)
                || keys.iter().all(|key| {
                    matches!(key, uqa_sql::ast::Expr::Column(column) if column_privilege_check(
                        &security,
                        column,
                        subject.as_ref(),
                        check,
                        &roles,
                        &memberships,
                    ))
                }),
        )
    }

    pub fn ensure_table_privilege(
        &self,
        name: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        let current_user = self.names.current_role();
        self.ensure_table_privilege_for(name, &current_user, privilege)
    }
    pub fn ensure_table_privilege_for(
        &self,
        name: &str,
        subject: &(impl RoleSubject + ?Sized),
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        let (relation, table) = self.bound_table_for_security(name)?;
        let bound = table.security();
        let roles = self.roles.role_definitions();
        let memberships = self.roles.role_memberships();
        let security = bound.resolve(&roles).map_err(SQLError::Internal)?;
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
        let current_user = self.names.current_role();
        self.ensure_column_privilege_for(name, column, &current_user, privilege)
    }
    pub fn ensure_column_privilege_for(
        &self,
        name: &str,
        column: &str,
        subject: &(impl RoleSubject + ?Sized),
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        let (relation, table) = self.bound_table_for_security(name)?;
        let bound = table.security();
        let roles = self.roles.role_definitions();
        let memberships = self.roles.role_memberships();
        let security = bound.resolve(&roles).map_err(SQLError::Internal)?;
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
        let current_user = self.names.current_role();
        self.ensure_any_column_privilege_for(name, &current_user, privilege)
    }
    pub fn ensure_any_column_privilege_for(
        &self,
        name: &str,
        subject: &(impl RoleSubject + ?Sized),
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        let (relation, table) = self.bound_table_for_security(name)?;
        let bound = table.security();
        let roles = self.roles.role_definitions();
        let memberships = self.roles.role_memberships();
        let security = bound.resolve(&roles).map_err(SQLError::Internal)?;
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
            return table
                .security()
                .resolve(&self.roles.role_definitions())
                .map(|security| security.role_owner)
                .map_err(SQLError::Internal);
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
    fn current_user_has_role_privileges(&self, target: &(impl RoleSubject + ?Sized)) -> bool {
        let current = self.names.current_role();
        let roles = self.roles.role_definitions();
        let memberships = self.roles.role_memberships();
        role_inherits(&roles, &memberships, &current, target)
    }
}
