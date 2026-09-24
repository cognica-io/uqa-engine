//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepare opaque physical index bindings once per retained registry and evaluate their keys in execution.

use crate::mutation::constraints::index_keys::{
    index_key_values, index_predicate_accepts, IndexExpressionContext,
};
use parking_lot::Mutex;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use uqa_core::{RelationIdentity, Value};
use uqa_sql::{
    ast::{ColumnDef, IndexKey, TableKeyConstraint},
    SQLError,
};
use uqa_storage::{
    document_store::Document, CatalogIndexRow, StorageBackendError, StorageBackendResult,
    ValueIndexKey,
};

type IndexRows = BTreeMap<RelationIdentity, CatalogIndexRow>;

pub fn column_value<'a>(document: &'a Document, column: &str) -> &'a Value {
    document.get(column).unwrap_or(&Value::Null)
}

struct PreparedIndex {
    table: String,
    method: String,
    keys: Vec<IndexKey>,
    definition: super::IndexDefinition,
}

#[derive(Default)]
pub struct PhysicalIndexDefinitions {
    indexes: BTreeMap<(String, String), PreparedIndex>,
}

struct CacheEntry {
    source: Arc<IndexRows>,
    prepared: Arc<PhysicalIndexDefinitions>,
}

/// Arc identity follows copy-on-write catalog publication, refresh and rollback; no independent invalidation counter can omit a mutation path.
#[derive(Default)]
pub struct PhysicalIndexCache {
    entry: Mutex<Option<CacheEntry>>,
}

impl PhysicalIndexCache {
    pub fn bind(
        &self,
        source: Arc<IndexRows>,
    ) -> StorageBackendResult<Arc<PhysicalIndexDefinitions>> {
        if let Some(entry) = self
            .entry
            .lock()
            .as_ref()
            .filter(|entry| Arc::ptr_eq(&entry.source, &source))
        {
            return Ok(entry.prepared.clone());
        }
        let prepared = Arc::new(PhysicalIndexDefinitions::prepare(&source)?);
        *self.entry.lock() = Some(CacheEntry {
            source,
            prepared: prepared.clone(),
        });
        Ok(prepared)
    }
}

impl PhysicalIndexDefinitions {
    fn prepare(rows: &IndexRows) -> StorageBackendResult<Self> {
        let mut indexes = BTreeMap::new();
        for row in rows.values() {
            let definition = super::index_definition(row)?;
            let identity = definition.catalog.as_ref().ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "index `{}` has no physical identity",
                    row.relation.qualified_name()
                ))
            })?;
            identity
                .validate(identity.table_object_id)
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
            if indexes
                .insert(
                    (row.table_name.clone(), identity.physical_key.clone()),
                    PreparedIndex {
                        table: row.table_name.clone(),
                        method: row.index_type.clone(),
                        keys: serde_json::from_str(&row.columns_json)?,
                        definition,
                    },
                )
                .is_some()
            {
                return Err(StorageBackendError::Other(
                    "duplicate physical index namespace".into(),
                ));
            }
        }
        Ok(Self { indexes })
    }

    pub fn indexable_fields(
        &self,
        table: &str,
        columns: &[ColumnDef],
        constraints: &[TableKeyConstraint],
    ) -> StorageBackendResult<Vec<ValueIndexKey>> {
        let mut fields = BTreeSet::new();
        for column in columns
            .iter()
            .filter(|column| column.primary_key || column.unique)
        {
            fields.insert(ValueIndexKey::Column(column.name.clone()));
        }
        for constraint in constraints {
            fields.extend(
                constraint
                    .columns
                    .iter()
                    .cloned()
                    .map(ValueIndexKey::Column),
            );
        }
        for ((_, physical_key), index) in &self.indexes {
            if !index.method.eq_ignore_ascii_case("btree") {
                continue;
            }
            if index.table != table {
                continue;
            }
            if index.keys.iter().any(|key| key.column().is_none()) {
                fields.insert(ValueIndexKey::Index(physical_key.clone()));
            }
            if let Some(IndexKey::Column(column)) = index.keys.first() {
                fields.insert(ValueIndexKey::Column(column.clone()));
            }
        }
        Ok(fields.into_iter().collect())
    }

    pub fn document_values(
        &self,
        expressions: IndexExpressionContext<'_>,
        table: &str,
        fields: &[ValueIndexKey],
        document: &Document,
    ) -> Result<BTreeMap<ValueIndexKey, Value>, SQLError> {
        fields
            .iter()
            .map(|field| {
                let value = match field {
                    ValueIndexKey::Column(column) => column_value(document, column).clone(),
                    ValueIndexKey::Index(key) => {
                        let index = self
                            .indexes
                            .get(&(table.to_owned(), key.clone()))
                            .ok_or_else(|| {
                                SQLError::Internal(format!(
                                    "missing physical index definition {key}"
                                ))
                            })?;
                        if index_predicate_accepts(
                            expressions,
                            table,
                            index.definition.predicate.as_deref(),
                            document,
                        )? {
                            Value::Row(index_key_values(expressions, table, &index.keys, document)?)
                        } else {
                            Value::Null
                        }
                    }
                };
                Ok((field.clone(), value))
            })
            .collect()
    }
}

pub mod rebuild;

#[cfg(test)]
mod tests;
