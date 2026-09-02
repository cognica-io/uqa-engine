//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable database ownership, ACL mutation, and privilege checks.

use std::collections::BTreeMap;

mod acl;
mod inquiry;

pub(crate) use acl::DatabaseAclPrivilege;
use acl::{
    grant_acl, requested_acl_privileges, revoke_acl, role_has_database_privilege,
    select_acl_grantor,
};
use uqa_sql::ast::{DatabaseRevokeBehavior, GrantDatabaseStmt};

use crate::engine_roles::{RoleDefinition, RoleMembership, RoleMembershipKey};
use crate::engine_state::DatabaseSecurity;
use crate::{
    CatalogFacade, Engine, SQLError, StorageBackendError, StorageBackendResult,
    DATABASE_SECURITY_METADATA_KEY,
};

pub(crate) const DATABASE_NAME: &str = "uqa";
pub(crate) const DATABASE_OID: i64 = 5;

impl Engine {
    pub(crate) fn grant_database_privileges(
        &self,
        statement: &GrantDatabaseStmt,
    ) -> Result<(), SQLError> {
        self.prepare_explicit_transaction_writer()?;
        self.synchronize_catalog_registries()
            .map_err(|error| SQLError::Internal(format!("load database privileges: {error}")))?;
        Self::resolve_database_grant_targets(&statement.databases)?;
        let grantees = statement
            .grantees
            .iter()
            .map(|role| self.resolve_role_reference(role))
            .collect::<Vec<_>>();
        let requested_grantor = statement
            .grantor
            .as_ref()
            .map(|role| self.resolve_role_reference(role));
        let current_user = self.current_user_name();
        let roles = self.durable.roles.read();
        validate_database_acl_roles(
            statement,
            &grantees,
            requested_grantor.as_deref(),
            &current_user,
            &roles,
        )?;
        let privileges = requested_acl_privileges(&statement.privileges)?;
        let memberships = self.durable.role_memberships.read();
        let current = self.durable.database_security.read().clone();
        let (next, grantable) = apply_database_acl(
            statement,
            &grantees,
            &privileges,
            &current_user,
            &roles,
            &memberships,
            &current,
        )?;
        let notice = (grantable != privileges.len())
            .then(|| database_acl_warning(statement.is_grant, grantable != 0, DATABASE_NAME));
        if next != current {
            self.persist_database_security(&next)?;
            *self.durable.database_security.write() = next;
            self.note_catalog_registry_changed();
        }
        drop(memberships);
        drop(roles);
        if let Some((level, message)) = notice {
            self.push_sql_notice(level, &message);
        }
        Ok(())
    }

    fn resolve_database_grant_targets(databases: &[String]) -> Result<(), SQLError> {
        for database in databases {
            if database != DATABASE_NAME {
                return Err(SQLError::Routine {
                    sqlstate: "3D000".into(),
                    message: format!("database \"{database}\" does not exist"),
                });
            }
        }
        Ok(())
    }

    fn persist_database_security(&self, security: &DatabaseSecurity) -> Result<(), SQLError> {
        let Some(catalog) = self.storage.catalog.as_ref() else {
            return Ok(());
        };
        let json = serde_json::to_string(security).map_err(|error| {
            SQLError::Internal(format!("serialize database privileges: {error}"))
        })?;
        catalog
            .set_metadata(DATABASE_SECURITY_METADATA_KEY, &json)
            .map_err(|error| SQLError::Internal(format!("persist database privileges: {error}")))
    }

    pub(crate) fn restore_database_security_from_metadata(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        let security = match catalog.get_metadata(DATABASE_SECURITY_METADATA_KEY)? {
            Some(json) => serde_json::from_str::<DatabaseSecurity>(&json)?,
            None => DatabaseSecurity::bootstrap(),
        };
        let roles = self.durable.roles.read();
        if !roles.contains_key(&security.role_owner) {
            return Err(StorageBackendError::Other(format!(
                "persisted database owner `{}` does not exist",
                security.role_owner
            )));
        }
        if let Some(acl) = security.acl.as_ref() {
            for entry in acl {
                let grantor = entry.grantor.as_deref().unwrap_or(&security.role_owner);
                if (entry.role != "PUBLIC" && !roles.contains_key(&entry.role))
                    || !roles.contains_key(grantor)
                {
                    return Err(StorageBackendError::Other(format!(
                        "persisted database ACL `{}` from `{grantor}` references a missing role",
                        entry.role
                    )));
                }
            }
        }
        drop(roles);
        *self.durable.database_security.write() = security;
        Ok(())
    }

    pub(crate) fn ensure_database_privilege(
        &self,
        role: &str,
        privilege: DatabaseAclPrivilege,
    ) -> Result<(), SQLError> {
        if role_has_database_privilege(
            &self.durable.database_security.read(),
            role,
            privilege,
            &self.durable.roles.read(),
            &self.durable.role_memberships.read(),
        ) {
            return Ok(());
        }
        let message = match privilege {
            DatabaseAclPrivilege::Temporary => {
                format!(
                    "permission denied to create temporary tables in database \"{DATABASE_NAME}\""
                )
            }
            DatabaseAclPrivilege::Connect | DatabaseAclPrivilege::Create => {
                format!("permission denied for database {DATABASE_NAME}")
            }
        };
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message,
        })
    }
}

fn apply_database_acl(
    statement: &GrantDatabaseStmt,
    grantees: &[String],
    privileges: &[DatabaseAclPrivilege],
    current_user: &str,
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    current: &DatabaseSecurity,
) -> Result<(DatabaseSecurity, usize), SQLError> {
    let grantors = privileges
        .iter()
        .map(|privilege| {
            (
                *privilege,
                select_acl_grantor(current, *privilege, current_user, roles, memberships),
            )
        })
        .collect::<Vec<_>>();
    let grantable = grantors
        .iter()
        .filter(|(_, grantor)| grantor.is_some())
        .count();
    let mut next = current.clone();
    for (privilege, grantor) in grantors {
        let Some(grantor) = grantor else {
            continue;
        };
        if statement.is_grant {
            grant_acl(
                &mut next,
                privilege,
                grantees,
                &grantor,
                statement.grant_option,
            );
        } else {
            revoke_acl(
                &mut next,
                privilege,
                grantees,
                &grantor,
                statement.grant_option_only,
                statement.revoke_behavior == DatabaseRevokeBehavior::Cascade,
            )?;
        }
    }
    Ok((next, grantable))
}

fn validate_database_acl_roles(
    statement: &GrantDatabaseStmt,
    grantees: &[String],
    requested_grantor: Option<&str>,
    current_user: &str,
    roles: &BTreeMap<String, RoleDefinition>,
) -> Result<(), SQLError> {
    for role in grantees {
        if role != "PUBLIC" && !roles.contains_key(role) {
            return Err(SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!("role \"{role}\" does not exist"),
            });
        }
    }
    if statement.is_grant && statement.grant_option && grantees.iter().any(|role| role == "PUBLIC")
    {
        return Err(SQLError::Routine {
            sqlstate: "0LP01".into(),
            message: "grant options can only be granted to roles".into(),
        });
    }
    if let Some(requested_grantor) = requested_grantor {
        if !roles.contains_key(requested_grantor) {
            return Err(SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!("role \"{requested_grantor}\" does not exist"),
            });
        }
        if requested_grantor != current_user {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "grantor must be current user".into(),
            });
        }
    }
    Ok(())
}

fn database_acl_warning(is_grant: bool, partial: bool, name: &str) -> (&'static str, String) {
    let message = match (is_grant, partial) {
        (true, true) => format!("not all privileges were granted for \"{name}\""),
        (true, false) => format!("no privileges were granted for \"{name}\""),
        (false, true) => format!("not all privileges could be revoked for \"{name}\""),
        (false, false) => format!("no privileges could be revoked for \"{name}\""),
    };
    ("WARNING", message)
}
