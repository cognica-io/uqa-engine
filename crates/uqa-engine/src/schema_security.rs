//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persist schema security through the active catalog provider.

use crate::{state::BoundSchemaSecurity, Engine, SQLError};
pub(crate) use uqa_sql::catalog::security::schema::SchemaAclPrivilege;

impl Engine {
    pub(crate) fn persist_schema_security(
        &self,
        name: &str,
        security: &BoundSchemaSecurity,
    ) -> Result<(), SQLError> {
        if let Some(catalog) = self.storage.catalog.as_ref() {
            catalog
                .save_schema_row(&security.row(name).into())
                .map_err(|error| {
                    SQLError::Internal(format!("persist schema privileges for `{name}`: {error}"))
                })?;
        }
        Ok(())
    }
}
