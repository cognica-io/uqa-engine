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
    catalog_acl::AclGrantee,
    catalog_role::BoundAclEntry,
    catalog_schema::{BoundSchemaRow, SchemaAclEntry, SchemaPrivileges, SchemaTupleIdentity},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundSchemaSecurity {
    pub tuple: Option<SchemaTupleIdentity>,
    pub role_owner: RoleIdentity,
    pub acl: Option<Vec<BoundAclEntry<SchemaPrivileges>>>,
}

impl BoundSchemaSecurity {
    pub fn namespace_oid(&self, name: &str) -> i64 {
        self.tuple
            .map_or_else(|| crate::catalog::oids::schema_oid(name), |tuple| tuple.oid)
    }
    pub fn owner(role_owner: RoleIdentity) -> Self {
        Self {
            tuple: None,
            role_owner,
            acl: None,
        }
    }

    pub fn bootstrap(name: &str) -> Self {
        let mut security = Self::from_row(BoundSchemaRow::bootstrap(name)).1;
        security.tuple = Some(SchemaTupleIdentity::initial(
            u32::try_from(crate::catalog::oids::schema_oid(name)).expect("bootstrap schema OID"),
        ));
        security
    }

    pub fn with_public_privileges(create: bool) -> Self {
        let owner = RoleIdentity::BOOTSTRAP;
        Self {
            tuple: None,
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
            tuple: None,
            role_owner: bind(&security.role_owner)?,
            acl: security
                .acl
                .as_ref()
                .map(|entries| {
                    entries
                        .iter()
                        .map(|entry| {
                            Ok(BoundAclEntry {
                                role: entry.role.role_name().map(bind).transpose()?,
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
                                role: entry
                                    .role
                                    .map(&name)
                                    .transpose()?
                                    .map_or(AclGrantee::Public, AclGrantee::Role),
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
        if self.tuple.is_some_and(|tuple| !tuple.is_valid()) {
            return Err("invalid schema catalog tuple identity".into());
        }
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
                tuple: row.tuple,
                role_owner: row.role_owner,
                acl: row.acl,
            },
        )
    }

    pub fn row(&self, name: impl Into<String>) -> BoundSchemaRow {
        BoundSchemaRow {
            name: name.into(),
            tuple: self.tuple,
            role_owner: self.role_owner,
            acl: self.acl.clone(),
        }
    }
}

#[cfg(test)]
mod tests;
