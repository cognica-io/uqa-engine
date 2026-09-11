//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind schema privilege lookups and persistence to the active catalog and session.

mod acl;
mod inquiry;

pub(crate) use acl::{role_has_schema_privilege, SchemaAclPrivilege};

use crate::state::SchemaSecurity;
use crate::{Engine, SQLError};

impl Engine {
    pub(crate) fn persist_schema_security(
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

use uqa_sql::catalog::security::schema::schema_security_with_public_privileges;
