//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Metadata and publication adapters for SQL-owned index declarations and execution-owned builds.
use crate::{capabilities::RelationResolution, Engine};
use uqa_execution::schema::indexes::registry::{IndexRegistryContext, IndexRegistryPublication};
use uqa_execution::schema::indexes::{
    creation::{IndexCreationContext, IndexCreationNamespace, IndexCreationPublication},
    IndexBuildContext,
};
use uqa_sql::{
    ast::{ColumnType, IndexKey},
    catalog::{errors::storage_error, index::IndexDefinition},
    schema::indexes::vectors::VectorIndexCatalog,
    SQLError,
};
use uqa_storage::vector_index::VectorIndexSpec;
use uqa_storage::{CatalogIndexRow, StorageBackendError, StorageBackendResult};
impl Engine {
    pub(crate) fn index_registry_context(&self) -> IndexRegistryContext<'_> {
        IndexRegistryContext {
            identities: self.catalog_identity_reservation_context(),
            publication: self,
            builds: self,
            vectors: self,
            tables: self,
            locks: self,
            lock_catalog: self,
        }
    }
    pub(crate) fn index_creation_context(&self) -> IndexCreationContext<'_> {
        let runtime = self.query_runtime_view();
        IndexCreationContext {
            creation: self.relation_creation_context(),
            namespace: self,
            names: self,
            schema: self,
            bindings: self,
            unique: IndexBuildContext {
                catalog: self,
                reads: self,
                expressions: self.constraint_execution_context().index_expressions(),
                memory: runtime.settings,
            },
            vectors: self,
            publication: self,
            notices: runtime.notices,
        }
    }
}

impl IndexRegistryPublication for Engine {
    fn persist_index(&self, row: &CatalogIndexRow) -> StorageBackendResult<()> {
        let relation = uqa_core::RelationIdentity::from_legacy_name(&row.table_name)
            .map_err(StorageBackendError::Other)?;
        let table = self
            .storage
            .tables
            .read()
            .get(&relation)
            .cloned()
            .ok_or_else(|| {
                StorageBackendError::Other(format!("index table `{}` disappeared", row.table_name))
            })?;
        if table.persistence != uqa_sql::ast::RelationPersistence::Temporary {
            if let Some(catalog) = &self.storage.catalog {
                catalog.save_catalog_index_row(row)?;
                self.note_table_catalog_changed();
            }
        }
        Ok(())
    }

    fn erase_index(&self, row: &CatalogIndexRow) -> StorageBackendResult<()> {
        let relation = uqa_core::RelationIdentity::from_legacy_name(&row.table_name)
            .map_err(StorageBackendError::Other)?;
        let temporary = self
            .storage
            .tables
            .read()
            .get(&relation)
            .is_some_and(|table| table.persistence == uqa_sql::ast::RelationPersistence::Temporary);
        if !temporary {
            if let Some(catalog) = &self.storage.catalog {
                catalog.drop_catalog_index(&row.relation)?;
                self.note_table_catalog_changed();
            }
        }
        Ok(())
    }

    fn publish_index(&self, row: CatalogIndexRow) {
        self.durable
            .catalog_indexes
            .write()
            .insert(row.relation.clone(), row);
        self.note_catalog_registry_changed();
    }

    fn forget_index(&self, relation: &uqa_core::RelationIdentity) {
        self.durable.catalog_indexes.write().remove(relation);
        self.note_catalog_registry_changed();
    }

    fn refresh_index_table(&self, table: &str) -> StorageBackendResult<()> {
        self.refresh_value_indexes_for_table(table)
    }
}
impl IndexCreationNamespace for Engine {
    fn ensure_table_owner(&self, table: &str) -> Result<(), SQLError> {
        Engine::ensure_table_owner(self, table).map(|_| ())
    }

    fn relation_exists(&self, name: &str) -> Result<bool, SQLError> {
        Ok(matches!(
            self.resolve_bound_relation_kind(name)?,
            RelationResolution::Found(_, _)
        ))
    }
}
impl VectorIndexCatalog for Engine {
    fn resolve_table_name(&self, name: &str) -> Result<Option<String>, SQLError> {
        self.try_resolve_table_name(name)
            .map_err(|error| storage_error("CREATE INDEX", &error))
    }
    fn column_type(&self, table: &str, column: &str) -> Result<Option<ColumnType>, SQLError> {
        Engine::column_type(self, table, column)
            .map_err(|error| storage_error("CREATE INDEX", &error))
    }
    fn vector_index_names(&self, table: &str, column: &str) -> Result<Vec<String>, SQLError> {
        self.vector_catalog_index_names_for_column(table, column)
            .map_err(|error| storage_error("CREATE INDEX", &error))
    }
}
impl IndexCreationPublication for Engine {
    fn add_text_field(
        &self,
        table: &str,
        column: &str,
        analyzer: Option<&str>,
    ) -> Result<(), SQLError> {
        // Admit a deferred statement writer before changing physical storage. Initial restoration already owns its backend transaction and has no session frame to promote.
        self.prepare_explicit_transaction_writer()?;
        self.add_fts_field_with_analyzer_inner(table, column.to_string(), analyzer)
            .map_err(|error| match self.runtime.cancellation.check() {
                Err(cancelled) => SQLError::Cancelled(cancelled),
                Ok(()) => SQLError::Internal(format!("add_fts_field: {error}")),
            })?;
        Ok(())
    }
    fn rebuild_vector_field(
        &self,
        table: &str,
        column: &str,
        dimensions: u32,
        spec: VectorIndexSpec,
    ) -> Result<bool, SQLError> {
        self.prepare_explicit_transaction_writer()?;
        self.rebuild_vector_field_in_transaction(table, column, dimensions, spec)
            .map_err(|error| storage_error("CREATE INDEX vector field", &error))
    }
    fn register_index(
        &self,
        name: &str,
        method: &str,
        table: &str,
        keys: &[IndexKey],
        options: &[(String, String)],
        definition: &IndexDefinition,
    ) -> Result<(), SQLError> {
        self.register_catalog_index_definition(name, method, table, keys, options, definition)
            .map_err(|error| storage_error("CREATE INDEX", &error))?;
        Ok(())
    }
}
