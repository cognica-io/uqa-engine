//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schema registries retain role incarnations while command views use current names.

use super::{
    role_bindings::{bind_role, role_name},
    SchemaSecurity,
};
use crate::catalog::roles::{RoleDefinition, RoleIdentity};
use std::collections::BTreeMap;
use uqa_core::{
    catalog_role::BoundAclEntry,
    catalog_schema::{BoundSchemaRow, SchemaAclEntry, SchemaPrivileges},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundSchemaSecurity {
    pub role_owner: RoleIdentity,
    pub acl: Option<Vec<BoundAclEntry<SchemaPrivileges>>>,
}

impl BoundSchemaSecurity {
    pub fn owner(role_owner: RoleIdentity) -> Self {
        Self {
            role_owner,
            acl: None,
        }
    }

    pub fn bootstrap(name: &str) -> Self {
        Self::from_row(BoundSchemaRow::bootstrap(name)).1
    }

    pub fn with_public_privileges(create: bool) -> Self {
        let owner = RoleIdentity::BOOTSTRAP;
        Self {
            role_owner: owner,
            acl: Some(vec![
                BoundAclEntry {
                    role: Some(owner),
                    grantor: owner,
                    privileges: SchemaPrivileges::ALL,
                    grant_options: SchemaPrivileges::default(),
                },
                BoundAclEntry {
                    role: None,
                    grantor: owner,
                    privileges: SchemaPrivileges {
                        usage: true,
                        create,
                    },
                    grant_options: SchemaPrivileges::default(),
                },
            ]),
        }
    }

    pub fn bind(
        security: &SchemaSecurity,
        roles: &BTreeMap<String, RoleDefinition>,
    ) -> Result<Self, String> {
        let bind = |name: &str| bind_role(roles, name, "schema");
        Ok(Self {
            role_owner: bind(&security.role_owner)?,
            acl: security
                .acl
                .as_ref()
                .map(|entries| {
                    entries
                        .iter()
                        .map(|entry| {
                            Ok(BoundAclEntry {
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
                        .collect::<Result<_, String>>()
                })
                .transpose()?,
        })
    }

    pub fn resolve(
        &self,
        roles: &BTreeMap<String, RoleDefinition>,
    ) -> Result<SchemaSecurity, String> {
        let name = |identity| role_name(roles, identity, "schema").map(str::to_owned);
        Ok(SchemaSecurity {
            role_owner: name(self.role_owner)?,
            acl: self
                .acl
                .as_ref()
                .map(|entries| {
                    entries
                        .iter()
                        .map(|entry| {
                            Ok(SchemaAclEntry {
                                role: entry.role.map_or_else(|| Ok("PUBLIC".into()), &name)?,
                                grantor: Some(name(entry.grantor)?),
                                privileges: entry.privileges,
                                grant_options: entry.grant_options,
                            })
                        })
                        .collect::<Result<_, String>>()
                })
                .transpose()?,
        })
    }

    pub fn validate(&self, roles: &BTreeMap<String, RoleDefinition>) -> Result<(), String> {
        role_name(roles, self.role_owner, "schema")?;
        for entry in self.acl.iter().flatten() {
            if let Some(grantee) = entry.role {
                role_name(roles, grantee, "schema")?;
            }
            role_name(roles, entry.grantor, "schema")?;
        }
        Ok(())
    }

    pub fn depends_on(&self, role: RoleIdentity) -> bool {
        self.role_owner == role
            || self
                .acl
                .iter()
                .flatten()
                .any(|entry| entry.role == Some(role) || entry.grantor == role)
    }

    pub fn from_row(row: BoundSchemaRow) -> (String, Self) {
        (
            row.name,
            Self {
                role_owner: row.role_owner,
                acl: row.acl,
            },
        )
    }

    pub fn row(&self, name: impl Into<String>) -> BoundSchemaRow {
        BoundSchemaRow {
            name: name.into(),
            role_owner: self.role_owner,
            acl: self.acl.clone(),
        }
    }
}

#[cfg(test)]
mod tests;
