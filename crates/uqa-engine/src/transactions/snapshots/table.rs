//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Undo data and its capture sources share an immutable generation, never live table owners.

use crate::{Engine, TableDataSnapshot, TableState};
use uqa_sql::SQLError;
use uqa_storage::{
    memory_capture::{MemoryCaptureCell, MemoryCaptureRegistry, MemoryCaptureSource},
    vector_index::VectorIndexes,
};

fn source<T: MemoryCaptureSource>(
    value: T,
    registry: Option<&MemoryCaptureRegistry>,
) -> Result<MemoryCaptureCell<T>, SQLError> {
    match registry {
        Some(registry) => registry
            .cell(value)
            .map_err(|error| Engine::storage_tx_error("snapshot source registration", &error)),
        None => Ok(MemoryCaptureCell::new(value)),
    }
}

impl TableDataSnapshot {
    pub(super) fn capture(
        table: &TableState,
        registry: Option<&MemoryCaptureRegistry>,
    ) -> Result<Self, SQLError> {
        // One writable view retains the original undo behavior; capture reads use its owning controlled producer.
        let documents = table
            .document_store
            .read()
            .writable_snapshot()
            .map_err(|error| {
                Engine::storage_tx_error("snapshot writable document store", &error)
            })?;
        let text = table
            .inverted_index
            .read()
            .writable_snapshot()
            .map_err(|error| {
                Engine::storage_tx_error("snapshot writable inverted index", &error)
            })?;
        let mut vectors = registry.map_or_else(VectorIndexes::default, VectorIndexes::memory);
        for (field, index) in table.vector_indexes.read().iter() {
            let index = index.writable_snapshot().map_err(|error| {
                Engine::storage_tx_error("snapshot writable vector index", &error)
            })?;
            vectors
                .live_mut()
                .and_then(|mut entries| entries.insert(field.clone(), index))
                .map_err(|error| {
                    Engine::storage_tx_error("snapshot vector registration", &error)
                })?;
        }
        let values = table
            .value_indexes
            .read()
            .iter()
            .filter(|(key, _)| matches!(key, uqa_storage::ValueIndexKey::Index(_)))
            .map(|(key, index)| (key.clone(), index.clone()))
            .collect::<std::collections::BTreeMap<_, _>>();
        use uqa_execution::catalog::index::value::ValueIndexes;
        let mut value_indexes = registry.map_or_else(ValueIndexes::default, ValueIndexes::memory);
        value_indexes.replace(values).map_err(|error| {
            Engine::storage_tx_error("snapshot value-index registration", &error)
        })?;
        macro_rules! metadata {
            ($field:ident) => {
                table
                    .$field
                    .source_snapshot(registry)
                    .map_err(|error| Engine::storage_tx_error("snapshot table metadata", &error))?
            };
        }
        Ok(Self {
            object_id: table.object_id(),
            catalog_oids: table.recorded_catalog_oids(),
            persistence: table.persistence,
            on_commit: table.on_commit,
            document_store: source(documents, registry)?,
            inverted_index: source(text, registry)?,
            vector_indexes: source(vectors, registry)?,
            value_indexes: source(value_indexes, registry)?,
            security: metadata!(security),
            storage_generation: metadata!(storage_generation),
            fts_fields: metadata!(fts_fields),
            columns: metadata!(columns),
            columns_declared: metadata!(columns_declared),
            next_id: metadata!(next_id),
            maps_integer_keys: metadata!(maps_integer_keys),
            analyzer: metadata!(analyzer),
            column_stats: metadata!(column_stats),
            column_stats_loaded: metadata!(column_stats_loaded),
            column_stats_dirty: metadata!(column_stats_dirty),
            statistics_maintenance: metadata!(statistics_maintenance),
            table_checks: metadata!(table_checks),
            foreign_keys: metadata!(foreign_keys),
            key_constraints: metadata!(key_constraints),
            hierarchy: metadata!(hierarchy),
            row_type_array_name: metadata!(row_type_array_name),
            dropped_attributes: metadata!(dropped_attributes),
            doc_count_cache: table
                .doc_count_cache
                .load(std::sync::atomic::Ordering::Acquire),
            doc_count_dirty: table
                .doc_count_dirty
                .load(std::sync::atomic::Ordering::Acquire),
        })
    }
}
