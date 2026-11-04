//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Transaction data snapshots and rollback restoration.

use super::{Engine, EngineDataSnapshot, SQLError, SessionStateSnapshot};
use crate::TableDataSnapshot;
use std::collections::BTreeMap;

#[cfg(test)]
mod tests;

impl Engine {
    pub(super) fn snapshot_transaction_data(
        &self,
    ) -> Result<Option<std::sync::Arc<EngineDataSnapshot>>, SQLError> {
        let mut tables = BTreeMap::new();
        let mut owners = BTreeMap::new();
        for (name, table) in self.storage.tables.read().iter() {
            if self.storage.backend.is_some()
                && table.persistence != uqa_sql::ast::RelationPersistence::Temporary
            {
                continue;
            }
            // Capture an independent writable view once. Its storage owner may share an immutable root until mutation; probing a separate read snapshot first would duplicate retention or copying before the statement starts.
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
                    security: table.security.snapshot(),
                    storage_generation: table.storage_generation(),
                    document_store,
                    inverted_index,
                    vector_indexes,
                    value_indexes: table
                        .value_indexes
                        .read()
                        .iter()
                        .filter(|(key, _)| matches!(key, uqa_storage::ValueIndexKey::Index(_)))
                        .map(|(key, index)| (key.clone(), index.clone()))
                        .collect(),
                    fts_fields: table.fts_fields.snapshot(),
                    columns: table.columns.snapshot(),
                    columns_declared: *table.columns_declared.read(),
                    next_id: *table.next_id.read(),
                    analyzer: table.analyzer.snapshot(),
                    column_stats: table.column_stats.snapshot(),
                    column_stats_loaded: *table.column_stats_loaded.read(),
                    column_stats_dirty: *table.column_stats_dirty.read(),
                    statistics_maintenance: table.statistics_maintenance.read().clone(),
                    table_checks: table.table_checks.snapshot(),
                    foreign_keys: table.foreign_keys.snapshot(),
                    key_constraints: table.key_constraints.snapshot(),
                    hierarchy: table.hierarchy.snapshot(),
                    row_type_array_name: table.row_type_array_name.snapshot(),
                    dropped_attributes: table.dropped_attributes.snapshot(),
                    doc_count_cache: table
                        .doc_count_cache
                        .load(std::sync::atomic::Ordering::Acquire),
                    doc_count_dirty: table
                        .doc_count_dirty
                        .load(std::sync::atomic::Ordering::Acquire),
                },
            );
            owners.insert(name.clone(), table.clone());
        }
        Ok(Some(std::sync::Arc::new(EngineDataSnapshot {
            table_owners: owners,
            data: std::sync::Arc::new(crate::EngineSnapshotData {
                tables,
                memory_membership: self.storage.tables.memory_snapshot(),
                durable: self.durable.snapshot(),
            }),
            foreign_memory_tables: self.extensions.foreign_memory_tables.read().clone(),
        })))
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
        if let Some(membership) = &snapshot.data.memory_membership {
            self.storage.tables.restore_memory(
                membership,
                snapshot
                    .table_owners
                    .iter()
                    .map(|(name, table)| (name.clone(), table.clone())),
            );
        } else {
            let mut tables = self.storage.tables.write();
            tables
                .retain(|name, table| {
                    snapshot.table_owners.contains_key(name)
                        || (self.storage.backend.is_some()
                            && table.persistence != uqa_sql::ast::RelationPersistence::Temporary)
                })
                .map_err(|err| Self::storage_tx_error("ROLLBACK table membership", &err))?;
            for (name, table) in &snapshot.table_owners {
                // DROP followed by same-name CREATE installs another object; undo must restore the captured incarnation as well as its contents.
                tables
                    .insert(name.clone(), table.clone())
                    .map_err(|err| Self::storage_tx_error("ROLLBACK table membership", &err))?;
            }
        }
        for (name, table_snapshot) in &snapshot.data.tables {
            let table = &snapshot.table_owners[name];
            table.security.restore(&table_snapshot.security);
            table_snapshot.restore_stores(table)?;
            table.fts_fields.restore(&table_snapshot.fts_fields);
            table.columns.restore(&table_snapshot.columns);
            *table.columns_declared.write() = table_snapshot.columns_declared;
            *table.next_id.write() = table_snapshot.next_id;
            table.analyzer.restore(&table_snapshot.analyzer);
            table.column_stats.restore(&table_snapshot.column_stats);
            *table.column_stats_loaded.write() = table_snapshot.column_stats_loaded;
            *table.column_stats_dirty.write() = table_snapshot.column_stats_dirty;
            table
                .statistics_maintenance
                .write()
                .clone_from(&table_snapshot.statistics_maintenance);
            table.table_checks.restore(&table_snapshot.table_checks);
            table.foreign_keys.restore(&table_snapshot.foreign_keys);
            table
                .key_constraints
                .restore(&table_snapshot.key_constraints);
            table.hierarchy.restore(&table_snapshot.hierarchy);
            table
                .row_type_array_name
                .restore(&table_snapshot.row_type_array_name);
            table
                .dropped_attributes
                .restore(&table_snapshot.dropped_attributes);
            table
                .value_indexes
                .write()
                .clone_from(&table_snapshot.value_indexes);
            table.doc_count_cache.store(
                table_snapshot.doc_count_cache,
                std::sync::atomic::Ordering::Release,
            );
            table.doc_count_dirty.store(
                table_snapshot.doc_count_dirty,
                std::sync::atomic::Ordering::Release,
            );
        }
        self.durable.restore(&snapshot.data.durable);
        self.clear_regtype_output_cache();
        *self.extensions.foreign_memory_tables.write() = snapshot.foreign_memory_tables.clone();
        Ok(())
    }
}

impl TableDataSnapshot {
    fn restore_stores(&self, table: &crate::TableState) -> Result<(), SQLError> {
        let document_store = self
            .document_store
            .writable_snapshot()
            .map_err(|err| Engine::storage_tx_error("ROLLBACK document restore", &err))?;
        let inverted_index = self
            .inverted_index
            .writable_snapshot()
            .map_err(|err| Engine::storage_tx_error("ROLLBACK FTS restore", &err))?;
        let mut vector_indexes = BTreeMap::new();
        for (field, index) in &self.vector_indexes {
            vector_indexes.insert(
                field.clone(),
                index
                    .writable_snapshot()
                    .map_err(|err| Engine::storage_tx_error("ROLLBACK vector restore", &err))?,
            );
        }
        *table.document_store.write() = document_store;
        *table.storage_generation.write() = self.storage_generation;
        *table.inverted_index.write() = inverted_index;
        table
            .vector_indexes
            .write()
            .live_mut()
            .and_then(|mut indexes| indexes.replace(vector_indexes))
            .map_err(|error| Engine::storage_tx_error("ROLLBACK vector registrations", &error))?;
        Ok(())
    }
}

impl Engine {
    pub(super) fn snapshot_session_state(&self) -> SessionStateSnapshot {
        let mut snapshot = self.session.state.read().clone();
        snapshot.portal_names = self.session.portals.lock().keys().cloned().collect();
        snapshot.command_overlays = self
            .session
            .command_mutation_overlays
            .lock()
            .iter()
            .map(uqa_execution::mutation::overlay::CommandMutationOverlay::checkpoint)
            .collect();
        snapshot
    }

    pub(super) fn restore_session_state(&self, snapshot: &SessionStateSnapshot) {
        let mut restored = snapshot.clone();
        for (overlay, checkpoint) in self
            .session
            .command_mutation_overlays
            .lock()
            .iter_mut()
            .zip(&snapshot.command_overlays)
        {
            overlay.restore(checkpoint);
        }
        restored.command_overlays.clear();
        let mut current = self.session.state.write();
        // Sequence DDL can restore an old object and its session value, while
        // DISCARD SEQUENCES must remain effective across every rollback boundary.
        if current.sequence_discard_generation != snapshot.sequence_discard_generation {
            restored.sequence_currvals = std::mem::take(&mut current.sequence_currvals);
            restored.last_sequence = current.last_sequence.take();
            restored.sequence_discard_generation = current.sequence_discard_generation;
        }
        *current = restored;
        drop(current);
        self.session
            .portals
            .lock()
            .retain(|name, _| snapshot.portal_names.contains(name));
    }

    /// Registry restoration must resolve graph names against the same boundary as the already rolled-back provider, before the remaining session state is restored.
    pub(super) fn restore_graph_transaction_overlay(&self, snapshot: &SessionStateSnapshot) {
        self.session
            .state
            .write()
            .graph_overlay
            .clone_from(&snapshot.graph_overlay);
    }
}
