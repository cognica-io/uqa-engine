//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable table-shaped relation ownership, access-control lists, and authorization checks.

use std::sync::Arc;

use uqa_sql::SQLError;

use uqa_sql::catalog::security::columns::role_has_column_privilege as column_privilege_check;
pub(crate) use uqa_sql::catalog::security::table::{rewrite_acl_owner, TableAclPrivilege};
use uqa_sql::catalog::security::table::{role_has_privilege, TablePrivilegeCheck};

use crate::roles::role_can_set;
use crate::schema_security::SchemaAclPrivilege;
use crate::state::SequenceSecurity;
use crate::{Engine, RelationIdentity, TableState};

pub(crate) use uqa_sql::catalog::security::table::role_has_table_privilege;

pub(crate) use uqa_sql::catalog::security::table::validate_table_security_invariants;

impl Engine {
    pub(crate) fn ensure_table_privilege(
        &self,
        name: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        let current_user = self.current_user_name();
        self.ensure_table_privilege_for(name, &current_user, privilege)
    }

    pub(crate) fn ensure_table_privilege_for(
        &self,
        name: &str,
        subject: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        let (relation, table) = self.bound_table_for_security(name)?;
        let security = table.security();
        let roles = self.durable.roles.read();
        let memberships = self.durable.role_memberships.read();
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

    /// Return declared columns for a mutation target whose canonical relation identity has already been authorized and bound.
    pub(crate) fn bound_table_column_names(&self, name: &str) -> Result<Vec<String>, SQLError> {
        let (_, table) = self.bound_table_for_security(name)?;
        let columns = table
            .columns
            .read()
            .iter()
            .map(|column| column.name.clone())
            .collect();
        Ok(columns)
    }

    pub(crate) fn ensure_column_privilege(
        &self,
        name: &str,
        column: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        let current_user = self.current_user_name();
        self.ensure_column_privilege_for(name, column, &current_user, privilege)
    }

    pub(crate) fn ensure_column_privilege_for(
        &self,
        name: &str,
        column: &str,
        subject: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        let (relation, table) = self.bound_table_for_security(name)?;
        let security = table.security();
        let roles = self.durable.roles.read();
        let memberships = self.durable.role_memberships.read();
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

    pub(crate) fn ensure_any_column_privilege(
        &self,
        name: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        let current_user = self.current_user_name();
        self.ensure_any_column_privilege_for(name, &current_user, privilege)
    }

    pub(crate) fn ensure_any_column_privilege_for(
        &self,
        name: &str,
        subject: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        let (relation, table) = self.bound_table_for_security(name)?;
        let security = table.security();
        let roles = self.durable.roles.read();
        let memberships = self.durable.role_memberships.read();
        let table_check = TablePrivilegeCheck {
            privilege,
            grant_option: false,
        };
        if role_has_privilege(&security, subject, table_check, &roles, &memberships)
            || table.columns.read().iter().any(|column| {
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

    pub(crate) fn ensure_view_privilege_for(
        &self,
        name: &str,
        view: &crate::StoredView,
        subject: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        let relation = RelationIdentity::from_legacy_name(name).map_err(SQLError::Internal)?;
        let security = view.security();
        let roles = self.durable.roles.read();
        let memberships = self.durable.role_memberships.read();
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
                    crate::StoredViewKind::View => "view",
                    crate::StoredViewKind::Materialized => "materialized view",
                },
                relation.name
            ),
        })
    }

    pub(crate) fn ensure_view_column_privilege_for(
        &self,
        name: &str,
        view: &crate::StoredView,
        column: &str,
        subject: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        let relation = RelationIdentity::from_legacy_name(name).map_err(SQLError::Internal)?;
        let security = view.security();
        let roles = self.durable.roles.read();
        let memberships = self.durable.role_memberships.read();
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
                    crate::StoredViewKind::View => "view",
                    crate::StoredViewKind::Materialized => "materialized view",
                },
                relation.name
            ),
        })
    }

    pub(crate) fn ensure_any_view_column_privilege_for(
        &self,
        name: &str,
        view: &crate::StoredView,
        subject: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        let security = view.security();
        let roles = self.durable.roles.read();
        let memberships = self.durable.role_memberships.read();
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

    pub(crate) fn maintenance_table_names(&self, operation: &str) -> Result<Vec<String>, SQLError> {
        self.synchronize_table_catalog()
            .map_err(|error| SQLError::Internal(format!("load tables for {operation}: {error}")))?;
        let tables = self
            .storage
            .tables
            .read()
            .iter()
            .map(|(relation, table)| (relation.clone(), table.security()))
            .collect::<Vec<_>>();
        let current_user = self.current_user_name();
        let roles = self.durable.roles.read();
        let memberships = self.durable.role_memberships.read();
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
            self.push_sql_notice(
                "WARNING",
                &format!("permission denied to {operation} \"{name}\", skipping it"),
            );
        }
        Ok(permitted)
    }

    fn bound_table_for_security(
        &self,
        name: &str,
    ) -> Result<(RelationIdentity, Arc<TableState>), SQLError> {
        let relation = RelationIdentity::from_legacy_name(name)
            .map_err(|error| SQLError::Internal(format!("resolve table `{name}`: {error}")))?;
        let table = self
            .storage
            .tables
            .read()
            .get(&relation)
            .cloned()
            .ok_or_else(|| SQLError::Internal(format!("table `{name}` disappeared")))?;
        Ok((relation, table))
    }

    pub(crate) fn ensure_table_owner(&self, name: &str) -> Result<String, SQLError> {
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

    pub(crate) fn ensure_table_drop_authority(&self, name: &str) -> Result<(), SQLError> {
        let (relation, table) = self.bound_table_for_security(name)?;
        let table_owner = table.role_owner();
        if self.current_user_has_role_privileges(&table_owner) {
            return Ok(());
        }
        if self
            .schema_security_for_privilege(&relation.schema)
            .is_some_and(|security| self.current_user_has_role_privileges(&security.role_owner))
        {
            return Ok(());
        }
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!("must be owner of table {}", relation.name),
        })
    }

    pub(crate) fn alter_table_role_owner(
        &self,
        name: &str,
        requested_owner: &str,
    ) -> Result<(), SQLError> {
        self.prepare_explicit_transaction_writer()?;
        let (relation, table) = self.bound_table_for_security(name)?;
        let current_owner = self.ensure_table_owner(name)?;
        let new_owner = self.resolve_role_reference(requested_owner);
        let current_user_is_superuser;
        {
            let roles = self.durable.roles.read();
            if !roles.contains_key(&new_owner) {
                return Err(SQLError::Routine {
                    sqlstate: "42704".into(),
                    message: format!("role \"{new_owner}\" does not exist"),
                });
            }
            let memberships = self.durable.role_memberships.read();
            let current_user = self.current_user_name();
            current_user_is_superuser = roles
                .get(&current_user)
                .is_some_and(|role| role.has(uqa_sql::ast::RoleAttribute::Superuser));
            if !role_can_set(&roles, &memberships, &current_user, &new_owner) {
                return Err(SQLError::Routine {
                    sqlstate: "42501".into(),
                    message: format!("must be able to SET ROLE \"{new_owner}\""),
                });
            }
        }
        if current_owner == new_owner {
            return Ok(());
        }
        if !current_user_is_superuser {
            self.require_schema_privilege(
                &relation.schema,
                &new_owner,
                SchemaAclPrivilege::Create,
            )?;
        }

        let sequence_updates =
            self.table_owned_sequence_owner_updates(table.object_id(), &new_owner)?;
        for (sequence, security) in &sequence_updates {
            self.persist_sequence_security(&sequence.qualified_name(), sequence, security)?;
        }
        let columns = table.columns.read().clone();
        let constraints = uqa_sql::ast::TableConstraintSet {
            columns_declared: Some(*table.columns_declared.read()),
            checks: table.table_checks.read().clone(),
            foreign_keys: table.foreign_keys.read().clone(),
            key_constraints: table.key_constraints.read().clone(),
            persistence: table.persistence,
            on_commit: table.on_commit,
            hierarchy: table.hierarchy.read().clone(),
        };
        let mut table_security = table.security();
        rewrite_acl_owner(&mut table_security, &new_owner);
        self.try_save_table_schema_with_components_and_security(
            name,
            &table,
            &columns,
            &constraints,
            &table_security,
        )
        .map_err(|error| SQLError::Internal(format!("persist table owner: {error}")))?;

        table.security.write().clone_from(&table_security);
        if !sequence_updates.is_empty() {
            let mut registry = self.durable.sequence_security.write();
            for (sequence, security) in sequence_updates {
                registry.insert(sequence, security);
            }
            drop(registry);
            self.note_catalog_registry_changed();
        }
        if table.persistence == uqa_sql::ast::RelationPersistence::Temporary {
            self.note_table_catalog_changed();
        }
        Ok(())
    }

    pub(crate) fn table_owned_sequence_owner_updates(
        &self,
        table_object_id: [u8; 16],
        new_owner: &str,
    ) -> Result<Vec<(RelationIdentity, SequenceSecurity)>, SQLError> {
        uqa_execution::schema::sequences::role_ownership::table_owned_sequence_owner_updates(
            self,
            table_object_id,
            new_owner,
        )
    }
}
