//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schema/catalog enumeration and schema lifecycle.

use super::{CatalogIndexRow, Engine, RelationIdentity, StorageBackendError, StorageBackendResult};

/// Namespaces the engine implements without a durable schema row: the
/// `PostgreSQL` system schemas and the Apache AGE catalog schema.
pub(crate) fn is_virtual_system_schema(name: &str) -> bool {
    matches!(name, "pg_catalog" | "information_schema" | "ag_catalog")
}

impl Engine {
    pub fn list_catalog_indexes(&self) -> StorageBackendResult<Vec<CatalogIndexRow>> {
        if let Some(snapshot) = self.query_catalog_snapshot.as_ref() {
            let mut out = snapshot
                .catalog_indexes
                .values()
                .cloned()
                .collect::<Vec<_>>();
            out.sort_by(|a, b| a.name.cmp(&b.name));
            return Ok(out);
        }
        self.synchronize_catalog_registries()?;
        let mut out: Vec<CatalogIndexRow> = self
            .durable
            .catalog_indexes
            .read()
            .values()
            .cloned()
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Create a durable schema catalog object. Returns `true` when a new
    /// schema was created and `false` only for `IF NOT EXISTS`.
    pub fn register_schema(&self, name: &str, if_not_exists: bool) -> StorageBackendResult<bool> {
        self.with_implicit_storage_transaction(|engine| {
            engine.synchronize_catalog_registries()?;
            engine
                .mutation_coordinator()
                .register_schema(name, if_not_exists)
        })
    }

    /// Drop an empty durable schema. `public` and the virtual system
    /// namespaces cannot be removed.
    pub fn drop_schema(&self, name: &str) -> StorageBackendResult<bool> {
        self.with_implicit_storage_transaction(|engine| engine.drop_schema_inner(name))
    }

    pub(crate) fn preflight_drop_schema(&self, name: &str) -> StorageBackendResult<bool> {
        self.synchronize_catalog_registries()?;
        if name == "public" || is_virtual_system_schema(name) {
            return Err(StorageBackendError::Other(format!(
                "schema `{name}` cannot be dropped"
            )));
        }
        if !self.durable.schemas.read().contains(name) {
            return Ok(false);
        }
        if !self.schema_is_empty(name) {
            return Err(StorageBackendError::Other(format!(
                "schema `{name}` is not empty"
            )));
        }
        Ok(true)
    }

    fn drop_schema_inner(&self, name: &str) -> StorageBackendResult<bool> {
        if !self.preflight_drop_schema(name)? {
            return Ok(false);
        }
        let mut schemas = self.durable.schemas.write();
        if let Some(catalog) = self.storage.catalog.as_ref() {
            catalog.drop_schema(name)?;
        }
        let removed = schemas.remove(name);
        drop(schemas);
        if removed {
            self.note_catalog_registry_changed();
        }
        Ok(removed)
    }

    pub fn has_schema(&self, name: &str) -> StorageBackendResult<bool> {
        self.synchronize_catalog_registries()?;
        Ok(self.durable.schemas.read().contains(name))
    }

    /// Whether `name` resolves as a namespace: a durable schema, a virtual
    /// system schema (`pg_catalog`, `information_schema`, `ag_catalog`), or
    /// the namespace a named graph owns.
    pub fn has_namespace(&self, name: &str) -> StorageBackendResult<bool> {
        self.synchronize_catalog_registries()?;
        Ok(is_virtual_system_schema(name)
            || self.durable.schemas.read().contains(name)
            || self.durable.graphs.read().contains_key(name))
    }

    pub(crate) fn validate_schema_name(name: &str) -> StorageBackendResult<()> {
        crate::engine_capabilities::validate_schema_name(name)
    }

    fn schema_is_empty(&self, schema: &str) -> bool {
        !self
            .storage
            .tables
            .read()
            .keys()
            .any(|relation| relation.schema == schema)
            && !self
                .durable
                .views
                .read()
                .keys()
                .any(|relation| relation.schema == schema)
            && !self
                .durable
                .sequences
                .read()
                .keys()
                .any(|relation| relation.schema == schema)
            && !self
                .durable
                .foreign_tables
                .read()
                .keys()
                .any(|relation| relation.schema == schema)
            && !self.durable.sql_user_functions.read().keys().any(|name| {
                RelationIdentity::from_legacy_name(name)
                    .map_or(true, |relation| relation.schema == schema)
            })
    }

    /// Return every registered schema in sorted order.
    pub fn list_schemas(&self) -> StorageBackendResult<Vec<String>> {
        if let Some(snapshot) = self.query_catalog_snapshot.as_ref() {
            return Ok(snapshot.schemas.iter().cloned().collect());
        }
        self.synchronize_catalog_registries()?;
        Ok(self.durable.schemas.read().iter().cloned().collect())
    }

    /// Local names of tables whose structural relation identity is owned by
    /// `schema`. No string-prefix inference participates in this lookup.
    pub fn tables_in_schema(&self, schema: &str) -> StorageBackendResult<Vec<String>> {
        self.synchronize_table_catalog()?;
        let mut out: Vec<String> = Vec::new();
        for relation in self.storage.tables.read().keys() {
            if relation.schema == schema {
                out.push(relation.name.clone());
            }
        }
        out.sort_unstable();
        Ok(out)
    }

    pub fn list_sequences(&self) -> StorageBackendResult<Vec<String>> {
        if let Some(snapshot) = self.query_catalog_snapshot.as_ref() {
            let mut out = snapshot
                .sequences
                .keys()
                .map(RelationIdentity::qualified_name)
                .collect::<Vec<_>>();
            out.sort_unstable();
            return Ok(out);
        }
        self.refresh_sequences_from_catalog()?;
        let mut out: Vec<String> = self
            .durable
            .sequences
            .read()
            .keys()
            .map(RelationIdentity::qualified_name)
            .collect();
        out.sort_unstable();
        Ok(out)
    }
}
