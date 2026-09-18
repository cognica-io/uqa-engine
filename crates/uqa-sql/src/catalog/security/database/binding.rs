//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Database ownership and ACL references retain role incarnations independently of names.

use super::{DatabaseAclEntry, DatabasePrivileges, DatabaseSecurity};
use crate::catalog::roles::{identity::RoleBinding, RoleDefinition, RoleIdentity};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundDatabaseAclEntry {
    /// An absent grantee represents PUBLIC, not a missing role.
    pub role: Option<RoleIdentity>,
    pub grantor: RoleIdentity,
    pub privileges: DatabasePrivileges,
    pub grant_options: DatabasePrivileges,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundDatabaseSecurity {
    pub role_owner: RoleIdentity,
    pub acl: Option<Vec<BoundDatabaseAclEntry>>,
}

fn role_name(
    roles: &BTreeMap<String, RoleDefinition>,
    identity: RoleIdentity,
) -> Result<&str, String> {
    if identity.oid <= 0 || u32::try_from(identity.oid).is_err() || identity.object_id == [0; 16] {
        return Err("invalid persisted database role identity".into());
    }
    roles
        .values()
        .find(|role| role.identity() == identity)
        .map(|role| role.name.as_str())
        .ok_or_else(|| {
            format!(
                "persisted database privileges reference missing role incarnation {}",
                identity.oid
            )
        })
}

impl BoundDatabaseSecurity {
    pub fn bootstrap() -> Self {
        Self {
            role_owner: RoleDefinition::bootstrap().identity(),
            acl: None,
        }
    }

    pub fn depends_on(&self, role: RoleIdentity) -> bool {
        self.role_owner == role
            || self.acl.as_ref().is_some_and(|entries| {
                entries
                    .iter()
                    .any(|entry| entry.role == Some(role) || entry.grantor == role)
            })
    }

    pub fn validate(&self, roles: &BTreeMap<String, RoleDefinition>) -> Result<(), String> {
        role_name(roles, self.role_owner)?;
        for entry in self.acl.iter().flatten() {
            if let Some(grantee) = entry.role {
                role_name(roles, grantee)?;
            }
            role_name(roles, entry.grantor)?;
        }
        Ok(())
    }

    pub fn bind(
        security: &DatabaseSecurity,
        roles: &BTreeMap<String, RoleDefinition>,
    ) -> Result<Self, String> {
        super::validate_stored_database_security(security, roles)?;
        let bind = |name: &str| {
            RoleBinding::from_definition(&roles[name])
                .map(|role| role.identity())
                .map_err(|error| error.to_string())
        };
        Ok(Self {
            role_owner: bind(&security.role_owner)?,
            acl: security
                .acl
                .as_ref()
                .map(|entries| {
                    entries
                        .iter()
                        .map(|entry| {
                            Ok(BoundDatabaseAclEntry {
                                role: (entry.role != "PUBLIC")
                                    .then(|| bind(&entry.role))
                                    .transpose()?,
                                grantor: bind(
                                    entry.grantor.as_deref().unwrap_or(&security.role_owner),
                                )?,
                                privileges: entry.privileges,
                                grant_options: entry.grant_options,
                            })
                        })
                        .collect::<Result<Vec<_>, String>>()
                })
                .transpose()?,
        })
    }

    /// Project current names without rewriting or rebinding the stored references.
    pub fn resolve(
        &self,
        roles: &BTreeMap<String, RoleDefinition>,
    ) -> Result<DatabaseSecurity, String> {
        let name = |identity| role_name(roles, identity).map(str::to_owned);
        Ok(DatabaseSecurity {
            role_owner: name(self.role_owner)?,
            acl: self
                .acl
                .as_ref()
                .map(|entries| {
                    entries
                        .iter()
                        .map(|entry| {
                            Ok(DatabaseAclEntry {
                                role: entry.role.map_or_else(|| Ok("PUBLIC".into()), &name)?,
                                grantor: Some(name(entry.grantor)?),
                                privileges: entry.privileges,
                                grant_options: entry.grant_options,
                            })
                        })
                        .collect::<Result<Vec<_>, String>>()
                })
                .transpose()?,
        })
    }
}

#[cfg(test)]
mod tests;
