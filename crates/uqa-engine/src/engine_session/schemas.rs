//! Schema/catalog enumeration and schema lifecycle.

use super::{CatalogIndexRow, Engine, RelationIdentity, StorageBackendError, StorageBackendResult};

impl Engine {
    pub fn list_catalog_indexes(&self) -> StorageBackendResult<Vec<CatalogIndexRow>> {
        self.synchronize_catalog_registries()?;
        let mut out: Vec<CatalogIndexRow> = self.catalog_indexes.read().values().cloned().collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Create a durable schema catalog object. Returns `true` when a new
    /// schema was created and `false` only for `IF NOT EXISTS`.
    pub fn register_schema(&self, name: &str, if_not_exists: bool) -> StorageBackendResult<bool> {
        self.with_implicit_storage_transaction(|engine| {
            engine.register_schema_inner(name, if_not_exists)
        })
    }

    fn register_schema_inner(&self, name: &str, if_not_exists: bool) -> StorageBackendResult<bool> {
        self.synchronize_catalog_registries()?;
        Self::validate_schema_name(name)?;
        let mut schemas = self.schemas.write();
        if schemas.contains(name) {
            if if_not_exists {
                return Ok(false);
            }
            return Err(StorageBackendError::Other(format!(
                "schema `{name}` already exists"
            )));
        }
        if let Some(catalog) = self.catalog.as_ref() {
            catalog.save_schema(name)?;
        }
        schemas.insert(name.to_string());
        drop(schemas);
        self.note_catalog_registry_changed();
        Ok(true)
    }

    /// Drop an empty durable schema. `public` and the virtual system
    /// namespaces cannot be removed.
    pub fn drop_schema(&self, name: &str) -> StorageBackendResult<bool> {
        self.with_implicit_storage_transaction(|engine| engine.drop_schema_inner(name))
    }

    pub(crate) fn preflight_drop_schema(&self, name: &str) -> StorageBackendResult<bool> {
        self.synchronize_catalog_registries()?;
        if matches!(name, "public" | "pg_catalog" | "information_schema") {
            return Err(StorageBackendError::Other(format!(
                "schema `{name}` cannot be dropped"
            )));
        }
        if !self.schemas.read().contains(name) {
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
        let mut schemas = self.schemas.write();
        if let Some(catalog) = self.catalog.as_ref() {
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
        Ok(self.schemas.read().contains(name))
    }

    pub(crate) fn validate_schema_name(name: &str) -> StorageBackendResult<()> {
        if name.is_empty() {
            return Err(StorageBackendError::Other(format!(
                "invalid schema name `{name}`"
            )));
        }
        if matches!(name, "pg_catalog" | "information_schema") {
            return Err(StorageBackendError::Other(format!(
                "schema name `{name}` is reserved"
            )));
        }
        Ok(())
    }

    fn schema_is_empty(&self, schema: &str) -> bool {
        !self
            .tables
            .read()
            .keys()
            .any(|relation| relation.schema == schema)
            && !self
                .views
                .read()
                .keys()
                .any(|relation| relation.schema == schema)
            && !self
                .sequences
                .read()
                .keys()
                .any(|relation| relation.schema == schema)
            && !self
                .foreign_tables
                .read()
                .keys()
                .any(|relation| relation.schema == schema)
            && !self.sql_user_functions.read().keys().any(|name| {
                RelationIdentity::from_legacy_name(name)
                    .map_or(true, |relation| relation.schema == schema)
            })
    }

    /// Sorted list of every registered schema. Mirrors the canonical UQA implementation's
    /// `Engine._tables.schemas`.
    pub fn list_schemas(&self) -> StorageBackendResult<Vec<String>> {
        self.synchronize_catalog_registries()?;
        Ok(self.schemas.read().iter().cloned().collect())
    }

    /// Local names of tables whose structural relation identity is owned by
    /// `schema`. No string-prefix inference participates in this lookup.
    pub fn tables_in_schema(&self, schema: &str) -> StorageBackendResult<Vec<String>> {
        self.synchronize_table_catalog()?;
        let mut out: Vec<String> = Vec::new();
        for relation in self.tables.read().keys() {
            if relation.schema == schema {
                out.push(relation.name.clone());
            }
        }
        out.sort_unstable();
        Ok(out)
    }

    pub fn list_sequences(&self) -> StorageBackendResult<Vec<String>> {
        self.refresh_sequences_from_catalog()?;
        let mut out: Vec<String> = self
            .sequences
            .read()
            .keys()
            .map(RelationIdentity::qualified_name)
            .collect();
        out.sort_unstable();
        Ok(out)
    }
}
