//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schema/catalog enumeration and schema lifecycle.

use super::{CatalogIndexRow, Engine, StorageBackendResult};

pub(crate) use uqa_sql::catalog::is_virtual_system_schema;

impl Engine {
    pub fn list_catalog_indexes(&self) -> StorageBackendResult<Vec<CatalogIndexRow>> {
        self.with_catalog_read_snapshot(Self::catalog_indexes_in_execution)
    }

    pub(crate) fn catalog_indexes_in_execution(
        &self,
    ) -> StorageBackendResult<Vec<CatalogIndexRow>> {
        if let Some(snapshot) = self.query_catalog_snapshot.as_ref() {
            let mut out = snapshot
                .catalog_indexes
                .values()
                .cloned()
                .collect::<Vec<_>>();
            out.sort_by(|a, b| a.relation.cmp(&b.relation));
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
        out.sort_by(|a, b| a.relation.cmp(&b.relation));
        Ok(out)
    }

    /// Create a durable schema catalog object. Returns `true` when a new
    /// schema was created and `false` only for `IF NOT EXISTS`.
    pub fn register_schema(&self, name: &str, if_not_exists: bool) -> StorageBackendResult<bool> {
        self.with_implicit_storage_transaction(|engine| {
            uqa_execution::schema::namespaces::register_api_schema(
                &engine.schema_creation_context(),
                name,
                if_not_exists,
            )
            .map_err(|error| uqa_storage::StorageBackendError::backend("CREATE SCHEMA", error))
        })
    }

    /// Drop an empty durable schema. Virtual system namespaces cannot be removed.
    pub fn drop_schema(&self, name: &str) -> StorageBackendResult<bool> {
        self.with_implicit_storage_transaction(|engine| {
            uqa_execution::schema::namespaces::removal::drop_empty_schema(
                &engine.empty_schema_removal_context(),
                name,
            )
        })
    }

    pub fn has_schema(&self, name: &str) -> StorageBackendResult<bool> {
        self.with_catalog_read_snapshot(|engine| {
            engine.synchronize_catalog_registries()?;
            Ok(engine.durable.schemas.read().contains_key(name))
        })
    }

    /// Whether `name` resolves as a namespace: a durable schema, a virtual
    /// system schema (`pg_catalog`, `information_schema`, `ag_catalog`), or
    /// the namespace a named graph owns.
    pub fn has_namespace(&self, name: &str) -> StorageBackendResult<bool> {
        self.with_catalog_read_snapshot(|engine| engine.has_namespace_in_execution(name))
    }

    pub(crate) fn has_namespace_in_execution(&self, name: &str) -> StorageBackendResult<bool> {
        self.synchronize_catalog_registries()?;
        Ok(is_virtual_system_schema(name)
            || self.durable.schemas.read().contains_key(name)
            || self.durable.graphs.read().contains_key(name))
    }

    pub(crate) fn validate_stored_schema_name(name: &str) -> StorageBackendResult<()> {
        crate::capabilities::validate_stored_schema_name(name)
    }

    pub(crate) fn schema_is_empty(&self, schema: &str) -> bool {
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
            && !self
                .durable
                .catalog_indexes
                .read()
                .keys()
                .any(|relation| relation.schema == schema)
            && !self
                .durable
                .domains
                .read()
                .values()
                .any(|domain| domain.identity.schema == schema)
            && !self.durable.sql_user_functions.read().keys().any(|name| {
                uqa_sql::schema::namespaces::removal::routine_name_occupies_schema(name, schema)
            })
    }

    /// Return every registered schema in sorted order.
    pub fn list_schemas(&self) -> StorageBackendResult<Vec<String>> {
        self.with_catalog_read_snapshot(|engine| {
            if let Some(snapshot) = engine.query_catalog_snapshot.as_ref() {
                return Ok(snapshot.schemas.keys().cloned().collect());
            }
            engine.synchronize_catalog_registries()?;
            Ok(engine.durable.schemas.read().keys().cloned().collect())
        })
    }

    /// Local names of tables whose structural relation identity is owned by
    /// `schema`. No string-prefix inference participates in this lookup.
    pub fn tables_in_schema(&self, schema: &str) -> StorageBackendResult<Vec<String>> {
        self.with_catalog_read_snapshot(|engine| engine.schema_tables_in_execution(schema))
    }

    pub(crate) fn schema_tables_in_execution(
        &self,
        schema: &str,
    ) -> StorageBackendResult<Vec<String>> {
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
        self.with_catalog_read_snapshot(|engine| Ok(engine.query_sequence_snapshot()?.names()))
    }
}
