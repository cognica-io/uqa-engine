//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Transaction data snapshots and rollback restoration.

use super::{Engine, EngineDataSnapshot, SQLError, SessionStateSnapshot};
use crate::TableDataSnapshot;
use std::collections::BTreeMap;

impl Engine {
    pub(super) fn snapshot_transaction_data(&self) -> Result<Option<EngineDataSnapshot>, SQLError> {
        let mut tables = BTreeMap::new();
        for (name, table) in self.storage.tables.read().iter() {
            if self.storage.backend.is_some()
                && table.persistence != uqa_sql::ast::RelationPersistence::Temporary
            {
                continue;
            }
            // Capture an already-writable deep copy once. Calling `snapshot`
            // and then probing `writable_snapshot` would allocate and discard
            // a second database-sized clone before the statement even starts.
            let document_store: std::sync::Arc<dyn uqa_storage::DocumentStore> =
                std::sync::Arc::from(table.document_store.read().writable_snapshot().map_err(
                    |err| Self::storage_tx_error("snapshot writable document store", &err),
                )?);
            let inverted_index: std::sync::Arc<dyn uqa_storage::InvertedIndex> =
                std::sync::Arc::from(table.inverted_index.read().writable_snapshot().map_err(
                    |err| Self::storage_tx_error("snapshot writable inverted index", &err),
                )?);
            let mut vector_indexes = BTreeMap::new();
            for (field, index) in table.vector_indexes.read().iter() {
                let snapshot: std::sync::Arc<dyn uqa_storage::VectorIndex> =
                    std::sync::Arc::from(index.writable_snapshot().map_err(|err| {
                        Self::storage_tx_error("snapshot writable vector index", &err)
                    })?);
                vector_indexes.insert(field.clone(), snapshot);
            }
            tables.insert(
                name.clone(),
                TableDataSnapshot {
                    state: table.clone(),
                    security: table.security(),
                    storage_generation: table.storage_generation(),
                    document_store,
                    inverted_index,
                    vector_indexes,
                    fts_fields: table.fts_fields.read().clone(),
                    columns: table.columns.read().clone(),
                    next_id: *table.next_id.lock(),
                    analyzer: table.analyzer.read().clone(),
                    column_stats: table.column_stats.read().clone(),
                    column_stats_loaded: table
                        .column_stats_loaded
                        .load(std::sync::atomic::Ordering::Acquire),
                    column_stats_dirty: table
                        .column_stats_dirty
                        .load(std::sync::atomic::Ordering::Acquire),
                    table_checks: table.table_checks.read().clone(),
                    foreign_keys: table.foreign_keys.read().clone(),
                    key_constraints: table.key_constraints.read().clone(),
                    hierarchy: table.hierarchy.read().clone(),
                    doc_count_cache: table
                        .doc_count_cache
                        .load(std::sync::atomic::Ordering::Acquire),
                    doc_count_dirty: table
                        .doc_count_dirty
                        .load(std::sync::atomic::Ordering::Acquire),
                },
            );
        }
        Ok(Some(EngineDataSnapshot {
            tables,
            durable: self.durable.snapshot(),
            foreign_memory_tables: self.extensions.foreign_memory_tables.read().clone(),
        }))
    }

    /// Memory-engine rollback path: snapshots only exist when no persistent
    /// backend is attached, except for session-local temporary tables. These
    /// store operations remain fallible so a restore failure cannot leave a
    /// half-restored engine behind a successful-looking rollback.
    pub(super) fn restore_transaction_data(
        &self,
        snapshot: &EngineDataSnapshot,
    ) -> Result<(), SQLError> {
        self.clear_bayesian_params_cache();
        {
            let mut tables = self.storage.tables.write();
            tables.retain(|name, table| {
                snapshot.tables.contains_key(name)
                    || (self.storage.backend.is_some()
                        && table.persistence != uqa_sql::ast::RelationPersistence::Temporary)
            });
            for (name, table_snapshot) in &snapshot.tables {
                tables
                    .entry(name.clone())
                    .or_insert_with(|| table_snapshot.state.clone());
            }
        }
        for table_snapshot in snapshot.tables.values() {
            let table = &table_snapshot.state;
            table.security.write().clone_from(&table_snapshot.security);
            let document_store = table_snapshot
                .document_store
                .writable_snapshot()
                .map_err(|err| Self::storage_tx_error("ROLLBACK document restore", &err))?;
            let inverted_index = table_snapshot
                .inverted_index
                .writable_snapshot()
                .map_err(|err| Self::storage_tx_error("ROLLBACK FTS restore", &err))?;
            let mut vector_indexes = BTreeMap::new();
            for (field, index) in &table_snapshot.vector_indexes {
                vector_indexes.insert(
                    field.clone(),
                    index
                        .writable_snapshot()
                        .map_err(|err| Self::storage_tx_error("ROLLBACK vector restore", &err))?,
                );
            }
            *table.document_store.write() = document_store;
            *table.storage_generation.write() = table_snapshot.storage_generation;
            *table.inverted_index.write() = inverted_index;
            *table.vector_indexes.write() = vector_indexes;
            table
                .fts_fields
                .write()
                .clone_from(&table_snapshot.fts_fields);
            table.columns.write().clone_from(&table_snapshot.columns);
            *table.next_id.lock() = table_snapshot.next_id;
            *table.analyzer.write() = table_snapshot.analyzer.clone();
            *table.column_stats.write() = table_snapshot.column_stats.clone();
            table.column_stats_loaded.store(
                table_snapshot.column_stats_loaded,
                std::sync::atomic::Ordering::Release,
            );
            table.column_stats_dirty.store(
                table_snapshot.column_stats_dirty,
                std::sync::atomic::Ordering::Release,
            );
            table
                .table_checks
                .write()
                .clone_from(&table_snapshot.table_checks);
            table
                .foreign_keys
                .write()
                .clone_from(&table_snapshot.foreign_keys);
            table
                .key_constraints
                .write()
                .clone_from(&table_snapshot.key_constraints);
            table
                .hierarchy
                .write()
                .clone_from(&table_snapshot.hierarchy);
            Self::value_indexes_clear(table);
            table.doc_count_cache.store(
                table_snapshot.doc_count_cache,
                std::sync::atomic::Ordering::Release,
            );
            table.doc_count_dirty.store(
                table_snapshot.doc_count_dirty,
                std::sync::atomic::Ordering::Release,
            );
        }
        self.durable.restore(&snapshot.durable);
        self.clear_regtype_output_cache();
        *self.extensions.foreign_memory_tables.write() = snapshot.foreign_memory_tables.clone();
        Ok(())
    }
}

impl Engine {
    pub(super) fn snapshot_session_state(&self) -> SessionStateSnapshot {
        let mut snapshot = self.session.state.read().clone();
        snapshot.portal_names = self.session.portals.lock().keys().cloned().collect();
        snapshot
    }

    pub(super) fn restore_session_state(&self, snapshot: &SessionStateSnapshot) {
        *self.session.state.write() = snapshot.clone();
        self.session
            .portals
            .lock()
            .retain(|name, _| snapshot.portal_names.contains(name));
    }
}
