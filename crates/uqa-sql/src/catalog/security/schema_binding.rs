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
use crate::catalog::roles::RoleDefinitions;
use crate::catalog::roles::RoleIdentity;
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
    pub const BUILTIN_NAMES: [&'static str; 3] = ["pg_catalog", "information_schema", "ag_catalog"];

    /// The initial namespace authority shared by memory and durable catalogs.
    pub fn initial_catalog() -> super::schema_registry::SchemaRegistry {
        let entries = std::iter::once(("public".into(), Self::bootstrap("public"))).chain(
            Self::BUILTIN_NAMES
                .map(|name| (name.into(), Self::builtin(name).expect("built-in schema"))),
        );
        super::schema_registry::SchemaRegistry::try_from_iter(entries)
            .expect("fixed bootstrap schema definitions fit native admission")
    }

    pub fn builtin(name: &str) -> Option<Self> {
        let mut security = match name {
            "pg_catalog" | "information_schema" => Self::with_public_privileges(false),
            "ag_catalog" => Self::bootstrap(name),
            _ => return None,
        };
        security.tuple = Self::bootstrap(name).tuple;
        Some(security)
    }

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
        Self::bootstrap_with_oid(
            name,
            u32::try_from(crate::catalog::oids::schema_oid(name)).expect("bootstrap schema OID"),
        )
    }

    /// A bootstrap schema whose namespace took `oid`, as a graph's schema takes the OID its creation allocated.
    pub fn bootstrap_with_oid(name: &str, oid: u32) -> Self {
        let mut security = Self::from_row(BoundSchemaRow::bootstrap(name)).1;
        security.tuple = Some(SchemaTupleIdentity::initial(oid));
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

    pub fn bind(security: &SchemaSecurity, roles: &RoleDefinitions) -> Result<Self, String> {
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

    pub fn resolve(&self, roles: &RoleDefinitions) -> Result<SchemaSecurity, String> {
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
                                    .map(name)
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

    pub fn validate(&self, roles: &RoleDefinitions) -> Result<(), String> {
        self.validate_with_control(roles, &uqa_core::memory::ProductionControl::uncontrolled())
            .map_err(|error| match error {
                crate::SQLError::Internal(message) => message,
                error => error.to_string(),
            })
    }

    pub fn validate_with_control(
        &self,
        roles: &RoleDefinitions,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<(), crate::SQLError> {
        crate::catalog::work::inline(self.validate_from(roles, control, &mut ()))
    }

    pub async fn validate_from_input(
        &self,
        roles: &RoleDefinitions,
        control: &uqa_core::memory::ProductionControl<'_>,
        input: &mut uqa_core::image_io::AsyncImageInput<'_>,
    ) -> Result<(), crate::SQLError> {
        self.validate_from(roles, control, input).await
    }

    async fn validate_from(
        &self,
        roles: &RoleDefinitions,
        control: &uqa_core::memory::ProductionControl<'_>,
        work: &mut impl crate::catalog::work::Work,
    ) -> Result<(), crate::SQLError> {
        use super::role_bindings::role_definition_from;
        control.check()?;
        if self.tuple.is_some_and(|tuple| !tuple.is_valid()) {
            return Err(crate::SQLError::Internal(
                "invalid schema catalog tuple identity".into(),
            ));
        }
        role_definition_from(roles, self.role_owner, "schema", control, work).await?;
        for entry in self.acl.iter().flatten() {
            work.checkpoint().await?;
            control.check()?;
            if let Some(grantee) = entry.role {
                role_definition_from(roles, grantee, "schema", control, work).await?;
            }
            role_definition_from(roles, entry.grantor, "schema", control, work).await?;
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
