//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed physical index policies, expression evaluation, and shared rebuilds.

use super::{
    BTreeMap, BTreeSet, ColumnValueIndex, DocId, SQLError, StorageBackendError,
    StorageBackendResult, TableState, Value, ValueIndexKey,
};
use crate::Engine;
use uqa_sql::ast::IndexKey;

fn storage_error(error: impl std::fmt::Display) -> StorageBackendError {
    StorageBackendError::Other(error.to_string())
}

impl Engine {
    pub(crate) fn value_indexable_fields(
        &self,
        table: &str,
    ) -> StorageBackendResult<Vec<ValueIndexKey>> {
        let Some(table_name) = self.try_resolve_table_name(table)? else {
            return Ok(Vec::new());
        };
        let mut fields = BTreeSet::new();
        if let Some(table) = self.try_table(&table_name)? {
            for column in table
                .columns
                .read()
                .iter()
                .filter(|column| column.primary_key || column.unique)
            {
                fields.insert(ValueIndexKey::Column(column.name.clone()));
            }
            for constraint in table.key_constraints.read().iter() {
                fields.extend(
                    constraint
                        .columns
                        .iter()
                        .cloned()
                        .map(ValueIndexKey::Column),
                );
            }
        }
        let rows = self
            .durable
            .catalog_indexes
            .read()
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for row in rows {
            if !row.index_type.eq_ignore_ascii_case("btree") {
                continue;
            }
            let applies = row.table_name == table_name
                || self
                    .loaded_table_hierarchy(
                        &crate::RelationIdentity::from_legacy_name(&row.table_name)
                            .map_err(storage_error)?,
                    )
                    .is_some_and(|hierarchy| hierarchy.partition_spec.is_some())
                    && self
                        .hierarchy_scan_tables(&row.table_name, true)
                        .map_err(storage_error)?
                        .contains(&table_name);
            if !applies {
                continue;
            }
            let keys: Vec<IndexKey> = serde_json::from_str(&row.columns_json)?;
            if keys.iter().any(|key| key.column().is_none()) {
                fields.insert(ValueIndexKey::Index(row.relation.qualified_name()));
            }
            if let Some(IndexKey::Column(column)) = keys.first() {
                fields.insert(ValueIndexKey::Column(column.clone()));
            }
        }
        Ok(fields.into_iter().collect())
    }

    pub(crate) fn value_index_document_values(
        &self,
        table: &str,
        fields: &[ValueIndexKey],
        document: &BTreeMap<String, Value>,
    ) -> Result<BTreeMap<ValueIndexKey, Value>, SQLError> {
        fields
            .iter()
            .map(|field| {
                let value = match field {
                    ValueIndexKey::Column(column) => {
                        document.get(column).cloned().unwrap_or(Value::Null)
                    }
                    ValueIndexKey::Index(name) => {
                        let relation = crate::RelationIdentity::from_legacy_name(name)
                            .map_err(SQLError::Internal)?;
                        let row = self
                            .durable
                            .catalog_indexes
                            .read()
                            .get(&relation)
                            .cloned()
                            .ok_or_else(|| {
                                SQLError::Internal(format!(
                                    "missing physical index definition {name}"
                                ))
                            })?;
                        let definition = crate::engine_catalog_indexes::index_definition(&row)
                            .map_err(|error| SQLError::Internal(error.to_string()))?;
                        if crate::sql::dml::index_predicate_accepts(
                            self,
                            table,
                            definition.predicate.as_deref(),
                            document,
                        )? {
                            let keys: Vec<IndexKey> = serde_json::from_str(&row.columns_json)
                                .map_err(|error| SQLError::Internal(error.to_string()))?;
                            Value::Row(crate::sql::dml::index_key_values(
                                self, table, &keys, document,
                            )?)
                        } else {
                            Value::Null
                        }
                    }
                };
                Ok((field.clone(), value))
            })
            .collect()
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
        let columns = fields
            .iter()
            .map(|field| match field {
                ValueIndexKey::Column(name) => Some(name.as_str()),
                ValueIndexKey::Index(_) => None,
            })
            .collect::<Option<Vec<_>>>();
        let mut result = fields
            .iter()
            .map(|_| Vec::with_capacity(ids.len()))
            .collect::<Vec<_>>();
        for chunk in ids.chunks(256) {
            if let Some(columns) = &columns {
                let store = table.document_store.read();
                let mut projected = store.get_fields_multi(chunk, columns)?;
                for id in chunk {
                    let Some(values) = projected.remove(id) else {
                        if store.get(*id)?.is_none() {
                            continue;
                        }
                        return Err(storage_error(format!(
                            "value-index rebuild for {table_name} lost document {id}"
                        )));
                    };
                    if values.len() != fields.len() {
                        return Err(storage_error(format!(
                            "value-index rebuild for {table_name} returned {} fields; expected {}",
                            values.len(),
                            fields.len()
                        )));
                    }
                    for (index, value) in values.into_iter().enumerate() {
                        result[index].push((*id, value));
                    }
                }
            } else {
                // Release document storage before evaluating user routines; callbacks can read the same relation.
                let documents = {
                    let store = table.document_store.read();
                    chunk
                        .iter()
                        .map(|id| store.get(*id).map(|document| (*id, document)))
                        .collect::<StorageBackendResult<Vec<_>>>()?
                };
                for (id, document) in documents {
                    let Some(document) = document else {
                        continue;
                    };
                    let values = self
                        .value_index_document_values(table_name, fields, &document)
                        .map_err(|error| StorageBackendError::backend("index expression", error))?;
                    for (index, field) in fields.iter().enumerate() {
                        result[index].push((
                            id,
                            values
                                .get(field)
                                .cloned()
                                .ok_or_else(|| storage_error("missing prepared physical key"))?,
                        ));
                    }
                }
            }
        }
        Ok(result)
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
