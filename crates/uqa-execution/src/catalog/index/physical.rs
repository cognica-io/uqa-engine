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
    /// Expression keys and predicates may invoke runtime callbacks during row publication. Their retained registry must be checked before deferring another index's visible changes.
    pub fn row_publication_uses_expressions(&self, table: &str) -> bool {
        self.indexes.values().any(|index| {
            index.table == table
                && (index.keys.iter().any(|key| key.column().is_none())
                    || index.definition.predicate.is_some())
        })
    }

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

    /// Every field with an accelerator: the search keys and the columns that indexes only carry.
    pub fn indexable_fields(
        &self,
        table: &str,
        columns: &[ColumnDef],
        constraints: &[TableKeyConstraint],
    ) -> StorageBackendResult<Vec<ValueIndexKey>> {
        let mut fields = self.search_fields(table, columns, constraints);
        fields.extend(self.included_columns(table));
        Ok(fields.into_iter().collect())
    }

    /// Fields whose accelerators answer predicates: the columns of declared keys, every plain key column of a btree index, and each btree index with an expression key.
    pub fn search_fields(
        &self,
        table: &str,
        columns: &[ColumnDef],
        constraints: &[TableKeyConstraint],
    ) -> BTreeSet<ValueIndexKey> {
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
        for (physical_key, index) in self.btree_indexes(table) {
            if index.keys.iter().any(|key| key.column().is_none()) {
                fields.insert(ValueIndexKey::Index(physical_key.clone()));
            }
            fields.extend(
                index
                    .keys
                    .iter()
                    .filter_map(IndexKey::column)
                    .map(|column| ValueIndexKey::Column(column.to_owned())),
            );
        }
        fields
    }

    /// Fields that btree indexes carry beside their keys and that are no search key. Their accelerators hold the stored values for index-only reads and answer no predicate.
    pub fn carried_fields(
        &self,
        table: &str,
        columns: &[ColumnDef],
        constraints: &[TableKeyConstraint],
    ) -> BTreeSet<ValueIndexKey> {
        let search = self.search_fields(table, columns, constraints);
        self.included_columns(table)
            .filter(|field| !search.contains(field))
            .collect()
    }

    fn btree_indexes<'a>(
        &'a self,
        table: &'a str,
    ) -> impl Iterator<Item = (&'a String, &'a PreparedIndex)> + 'a {
        self.indexes
            .iter()
            .filter(move |(_, index)| {
                index.method.eq_ignore_ascii_case("btree") && index.table == table
            })
            .map(|((_, physical_key), index)| (physical_key, index))
    }

    fn included_columns<'a>(&'a self, table: &'a str) -> impl Iterator<Item = ValueIndexKey> + 'a {
        self.btree_indexes(table).flat_map(|(_, index)| {
            index
                .definition
                .included_columns
                .iter()
                .cloned()
                .map(ValueIndexKey::Column)
        })
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
                        expression_value(index, expressions, table, document)?
                    }
                };
                Ok((field.clone(), value))
            })
            .collect()
    }

    /// UNIQUE expression keys of a command-visible row. A rejected partial-index predicate keeps a NULL marker distinct from every row-valued key.
    pub fn command_expression_values(
        &self,
        expressions: IndexExpressionContext<'_>,
        table: &str,
        document: &Document,
    ) -> Result<Document, SQLError> {
        self.btree_indexes(table)
            .filter(|(_, index)| {
                index.definition.unique && index.keys.iter().any(|key| key.column().is_none())
            })
            .map(|(key, index)| {
                expression_value(index, expressions, table, document)
                    .map(|value| (key.clone(), value))
            })
            .collect()
    }
}

fn expression_value(
    index: &PreparedIndex,
    expressions: IndexExpressionContext<'_>,
    table: &str,
    document: &Document,
) -> Result<Value, SQLError> {
    if index_predicate_accepts(
        expressions,
        table,
        index.definition.predicate.as_deref(),
        document,
    )? {
        Ok(Value::Row(
            index_key_values(expressions, table, &index.keys, document)?.into(),
        ))
    } else {
        Ok(Value::Null)
    }
}

mod comparison;
pub mod rebuild;
mod update;

#[cfg(test)]
mod tests;
