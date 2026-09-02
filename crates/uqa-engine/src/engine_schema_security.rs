//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable schema ownership, ACL mutation, and privilege checks.

use std::collections::BTreeMap;

mod acl;
mod inquiry;

use acl::{grant_acl, requested_acl_privileges, revoke_acl, select_acl_grantor};
pub(crate) use acl::{role_has_schema_privilege, SchemaAclPrivilege};
use uqa_sql::ast::{GrantSchemaStmt, SchemaRevokeBehavior};
use uqa_storage::{SchemaAclEntry, SchemaPrivileges};

use crate::engine_roles::{RoleDefinition, RoleMembership, RoleMembershipKey};
use crate::engine_state::SchemaSecurity;
use crate::{Engine, SQLError};

impl Engine {
    pub(crate) fn grant_schema_privileges(
        &self,
        statement: &GrantSchemaStmt,
    ) -> Result<(), SQLError> {
        self.prepare_explicit_transaction_writer()?;
        self.synchronize_catalog_registries()
            .map_err(|error| SQLError::Internal(format!("load schemas for privileges: {error}")))?;
        let targets = self.resolve_schema_grant_targets(&statement.schemas)?;
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
        validate_schema_acl_roles(
            statement,
            &grantees,
            requested_grantor.as_deref(),
            &current_user,
            &roles,
        )?;
        let privileges = requested_acl_privileges(&statement.privileges)?;
        let memberships = self.durable.role_memberships.read();
        let mut registry = self.durable.schemas.write();
        let mut updates = Vec::new();
        let mut notices = Vec::new();
        for name in &targets {
            let current = registry.get(name).cloned().ok_or_else(|| {
                SQLError::Internal(format!("schema `{name}` has no security metadata"))
            })?;
            let (next, grantable) = apply_schema_acl(
                statement,
                &grantees,
                &privileges,
                &current_user,
                &roles,
                &memberships,
                &current,
            )?;
            if grantable != privileges.len() {
                notices.push(schema_acl_warning(statement.is_grant, grantable != 0, name));
            }
            if next != current {
                updates.push((name.clone(), next));
            }
        }
        for (name, security) in &updates {
            self.persist_schema_security(name, security)?;
        }
        let changed = !updates.is_empty();
        for (name, security) in updates {
            registry.insert(name, security);
        }
        drop(registry);
        drop(memberships);
        drop(roles);
        for (level, message) in notices {
            self.push_sql_notice(level, &message);
        }
        if changed {
            self.note_catalog_registry_changed();
        }
        Ok(())
    }

    fn resolve_schema_grant_targets(&self, schemas: &[String]) -> Result<Vec<String>, SQLError> {
        let registry = self.durable.schemas.read();
        let mut targets = Vec::with_capacity(schemas.len());
        for schema in schemas {
            if !registry.contains_key(schema) {
                return Err(SQLError::Routine {
                    sqlstate: "3F000".into(),
                    message: format!("schema \"{schema}\" does not exist"),
                });
            }
            if !targets.contains(schema) {
                targets.push(schema.clone());
            }
        }
        Ok(targets)
    }

    fn persist_schema_security(
        &self,
        name: &str,
        security: &SchemaSecurity,
    ) -> Result<(), SQLError> {
        if let Some(catalog) = self.storage.catalog.as_ref() {
            catalog
                .save_schema_row(&security.row(name))
                .map_err(|error| {
                    SQLError::Internal(format!("persist schema privileges for `{name}`: {error}"))
                })?;
        }
        Ok(())
    }

    pub(crate) fn schema_has_privilege_for_role(
        &self,
        schema: &str,
        role: &str,
        privilege: SchemaAclPrivilege,
    ) -> bool {
        let Some(security) = self.schema_security_for_privilege(schema) else {
            return false;
        };
        role_has_schema_privilege(
            &security,
            role,
            privilege,
            &self.durable.roles.read(),
            &self.durable.role_memberships.read(),
        )
    }

    pub(crate) fn schema_security_for_privilege(&self, schema: &str) -> Option<SchemaSecurity> {
        if let Some(security) = self.durable.schemas.read().get(schema) {
            return Some(security.clone());
        }
        match schema {
            "pg_catalog" | "information_schema" => {
                Some(schema_security_with_public_privileges(false))
            }
            "ag_catalog" => Some(SchemaSecurity::legacy("ag_catalog")),
            name if name == self.temporary_schema_name() => {
                Some(schema_security_with_public_privileges(true))
            }
            name if self.durable.graphs.read().contains_key(name) => {
                Some(SchemaSecurity::legacy(name))
            }
            _ => None,
        }
    }

    pub(crate) fn require_schema_privilege(
        &self,
        schema: &str,
        role: &str,
        privilege: SchemaAclPrivilege,
    ) -> Result<(), SQLError> {
        if self.schema_has_privilege_for_role(schema, role, privilege) {
            return Ok(());
        }
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!("permission denied for schema {schema}"),
        })
    }
}

fn schema_security_with_public_privileges(create: bool) -> SchemaSecurity {
    let role_owner = "uqa".to_string();
    SchemaSecurity {
        role_owner: role_owner.clone(),
        acl: Some(vec![
            SchemaAclEntry {
                role: role_owner.clone(),
                grantor: Some(role_owner.clone()),
                privileges: SchemaPrivileges::ALL,
                grant_options: SchemaPrivileges::default(),
            },
            SchemaAclEntry {
                role: "PUBLIC".into(),
                grantor: Some(role_owner),
                privileges: SchemaPrivileges {
                    usage: true,
                    create,
                },
                grant_options: SchemaPrivileges::default(),
            },
        ]),
    }
}

fn apply_schema_acl(
    statement: &GrantSchemaStmt,
    grantees: &[String],
    privileges: &[SchemaAclPrivilege],
    current_user: &str,
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    current: &SchemaSecurity,
) -> Result<(SchemaSecurity, usize), SQLError> {
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
                statement.revoke_behavior == SchemaRevokeBehavior::Cascade,
            )?;
        }
    }
    Ok((next, grantable))
}

fn validate_schema_acl_roles(
    statement: &GrantSchemaStmt,
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

fn schema_acl_warning(is_grant: bool, partial: bool, name: &str) -> (&'static str, String) {
    let message = match (is_grant, partial) {
        (true, true) => format!("not all privileges were granted for \"{name}\""),
        (true, false) => format!("no privileges were granted for \"{name}\""),
        (false, true) => format!("not all privileges could be revoked for \"{name}\""),
        (false, false) => format!("no privileges could be revoked for \"{name}\""),
    };
    ("WARNING", message)
}
