//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Initial schema role conversion joins the caller's complete restoration transaction.

use std::collections::BTreeMap;
use uqa_sql::catalog::{
    roles::RoleDefinition,
    security::{BoundSchemaSecurity, SchemaSecurity},
};
use uqa_storage::{CatalogFacade, SchemaRow, StorageBackendError, StorageBackendResult};

pub fn restore(
    catalog: &dyn CatalogFacade,
    roles: &BTreeMap<String, RoleDefinition>,
    allow_migration: bool,
) -> StorageBackendResult<BTreeMap<String, BoundSchemaSecurity>> {
    let mut restored = BTreeMap::new();
    let mut migrations = Vec::new();
    for row in catalog.load_schema_rows()? {
        uqa_sql::schema::namespaces::validate_stored_schema_name(row.name())
            .map_err(StorageBackendError::Other)?;
        let (name, security) = match row {
            SchemaRow::Bound(row) => BoundSchemaSecurity::from_row(row),
            SchemaRow::Legacy(row) => {
                if !allow_migration {
                    return Err(StorageBackendError::Other(
                        "schema security requires initial catalog migration".into(),
                    ));
                }
                let (name, security) = SchemaSecurity::from_row(row);
                let bound = BoundSchemaSecurity::bind(&security, roles)
                    .map_err(StorageBackendError::Other)?;
                migrations.push(bound.row(&name).into());
                (name, bound)
            }
        };
        security
            .validate(roles)
            .map_err(StorageBackendError::Other)?;
        if restored.insert(name.clone(), security).is_some() {
            return Err(StorageBackendError::Other(format!(
                "duplicate schema security for `{name}`"
            )));
        }
    }
    for row in &migrations {
        catalog.save_schema_row(row)?;
    }
    Ok(restored)
}

#[cfg(test)]
mod tests;
