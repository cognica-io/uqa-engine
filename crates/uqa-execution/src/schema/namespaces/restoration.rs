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

const BUILTIN_INITIALIZED: &str = "sql_builtin_schema_catalog_initialized";

pub fn restore(
    catalog: &dyn CatalogFacade,
    roles: &BTreeMap<String, RoleDefinition>,
    allow_migration: bool,
) -> StorageBackendResult<BTreeMap<String, BoundSchemaSecurity>> {
    let mut restored = BTreeMap::new();
    let mut migrations = Vec::new();
    let mut oids = std::collections::BTreeSet::new();
    let mut identities = std::collections::BTreeSet::new();
    for row in catalog.load_schema_rows()? {
        uqa_sql::schema::namespaces::validate_stored_schema_name(row.name())
            .map_err(StorageBackendError::Other)?;
        let legacy = matches!(&row, SchemaRow::Legacy(_));
        let (name, mut security) = match row {
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
                (name, bound)
            }
        };
        security
            .validate(roles)
            .map_err(StorageBackendError::Other)?;
        let missing_identity = security.tuple.is_none();
        if missing_identity {
            if !allow_migration {
                return Err(StorageBackendError::Other(
                    "schema identity requires initial catalog migration".into(),
                ));
            }
            security.tuple = BoundSchemaSecurity::bootstrap(&name).tuple;
        }
        let tuple = security.tuple.expect("schema identity restored");
        if !oids.insert(tuple.oid) || !identities.insert(tuple.object_id) {
            return Err(StorageBackendError::Other(format!(
                "duplicate schema catalog identity for `{name}`"
            )));
        }
        if legacy || missing_identity {
            migrations.push(security.row(&name).into());
        }
        if restored.insert(name.clone(), security).is_some() {
            return Err(StorageBackendError::Other(format!(
                "duplicate schema security for `{name}`"
            )));
        }
    }
    // Initialize once so a later deliberate drop or rename is not undone on reopen.
    let initialize_builtins = catalog.get_metadata(BUILTIN_INITIALIZED)?.is_none();
    if initialize_builtins {
        if !allow_migration {
            return Err(StorageBackendError::Other(
                "built-in schemas require initial catalog migration".into(),
            ));
        }
        for name in BoundSchemaSecurity::BUILTIN_NAMES {
            if restored.contains_key(name) {
                continue;
            }
            let security = BoundSchemaSecurity::builtin(name).expect("built-in schema");
            security
                .validate(roles)
                .map_err(StorageBackendError::Other)?;
            let tuple = security.tuple.expect("built-in schema identity");
            if !oids.insert(tuple.oid) || !identities.insert(tuple.object_id) {
                return Err(StorageBackendError::Other(format!(
                    "duplicate schema catalog identity for `{name}`"
                )));
            }
            migrations.push(security.row(name).into());
            restored.insert(name.into(), security);
        }
    }
    for row in &migrations {
        catalog.save_schema_row(row)?;
    }
    if initialize_builtins {
        catalog.set_metadata(BUILTIN_INITIALIZED, "true")?;
    }
    Ok(restored)
}

#[cfg(test)]
mod tests;
