//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain document guards, index definitions and physical publication handles for execution-owned index work.

use super::{
    BTreeMap, ColumnValueIndex, DocId, SQLError, StorageBackendResult, TableState, Value,
    ValueIndexKey,
};
use crate::Engine;
use uqa_execution::catalog::index::physical::PhysicalIndexDefinitions;

struct RetainedIndexDocuments<'a>(&'a TableState);

impl uqa_execution::catalog::index::physical::rebuild::IndexDocuments
    for RetainedIndexDocuments<'_>
{
    fn read(&self) -> Box<dyn std::ops::Deref<Target = Box<dyn uqa_storage::DocumentStore>> + '_> {
        Box::new(self.0.document_store.read())
    }
}

impl Engine {
    fn physical_index_definitions(
        &self,
    ) -> StorageBackendResult<std::sync::Arc<PhysicalIndexDefinitions>> {
        self.runtime
            .physical_index_cache
            .bind(self.durable.catalog_indexes.snapshot())
    }

    pub(crate) fn value_indexable_fields(
        &self,
        table: &str,
    ) -> StorageBackendResult<Vec<ValueIndexKey>> {
        let Some(name) = self.try_resolve_table_name(table)? else {
            return Ok(Vec::new());
        };
        let Some(state) = self.try_table(&name)? else {
            return Ok(Vec::new());
        };
        let columns = state.columns.snapshot();
        let constraints = state.key_constraints.snapshot();
        self.physical_index_definitions()?
            .indexable_fields(&name, &columns, &constraints)
    }

    pub(crate) fn value_index_document_values(
        &self,
        table: &str,
        fields: &[ValueIndexKey],
        document: &BTreeMap<String, Value>,
    ) -> Result<BTreeMap<ValueIndexKey, Value>, SQLError> {
        self.physical_index_definitions()
            .map_err(|error| uqa_sql::catalog::errors::storage_error("index definitions", &error))?
            .document_values(
                self.constraint_execution_context().index_expressions(),
                table,
                fields,
                document,
            )
    }

    pub(super) fn project_value_index_rows(
        &self,
        table: &TableState,
        table_name: &str,
        field: &ValueIndexKey,
        ids: &[DocId],
    ) -> StorageBackendResult<Vec<(DocId, Value)>> {
        Ok(self
            .project_value_index_rows_many(table, table_name, std::slice::from_ref(field), ids)?
            .pop()
            .unwrap_or_default())
    }

    fn project_value_index_rows_many(
        &self,
        table: &TableState,
        table_name: &str,
        fields: &[ValueIndexKey],
        ids: &[DocId],
    ) -> StorageBackendResult<Vec<Vec<(DocId, Value)>>> {
        uqa_execution::catalog::index::physical::rebuild::project(
            &RetainedIndexDocuments(table),
            self.physical_index_definitions()?.as_ref(),
            self.constraint_execution_context().index_expressions(),
            table_name,
            fields,
            ids,
        )
    }

    pub(super) fn rebuild_persistent_value_indexes(
        &self,
        table_name: &str,
        table: &TableState,
        fields: &[ValueIndexKey],
        backend: &dyn uqa_storage::PersistentStorageBackend,
    ) -> StorageBackendResult<()> {
        if fields.is_empty() {
            return Ok(());
        }
        let ids = table.document_store.read().doc_ids()?;
        let values = self.project_value_index_rows_many(table, table_name, fields, &ids)?;
        let replacements = fields
            .iter()
            .zip(&values)
            .map(|(field, values)| (field, values.as_slice()))
            .collect::<Vec<_>>();
        backend.replace_btree_indexes(table_name, &replacements)?;
        let mut indexes = table.value_indexes.write();
        for (field, values) in fields.iter().zip(values) {
            indexes
                .entry(field.clone())
                .or_insert_with(|| ColumnValueIndex::build(field.name(), values.into_iter()));
        }
        Ok(())
    }

    pub(crate) fn persistent_value_index_document_values(
        &self,
        table: &str,
        document: &BTreeMap<String, Value>,
    ) -> Result<Option<BTreeMap<ValueIndexKey, Value>>, SQLError> {
        if !self
            .storage
            .backend
            .as_ref()
            .is_some_and(|backend| backend.persists_btree_indexes())
        {
            return Ok(None);
        }
        let table_name = self
            .try_resolve_table_name(table)
            .map_err(|error| SQLError::Internal(error.to_string()))?
            .ok_or_else(|| SQLError::UnknownTable(table.into()))?;
        if self.value_index_table_is_temporary(&table_name)? {
            return Ok(None);
        }
        let fields = self
            .value_indexable_fields(&table_name)
            .map_err(|error| SQLError::Internal(error.to_string()))?;
        self.value_index_document_values(&table_name, &fields, document)
            .map(Some)
    }
}
