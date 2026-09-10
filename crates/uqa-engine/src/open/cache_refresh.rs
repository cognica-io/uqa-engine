//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Refresh only the dependencies changed in a pinned durable snapshot.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::Ordering;

use uqa_storage::{CatalogCacheRevisions, StorageBackendError, StorageBackendResult};

use super::Engine;

fn changed_names(
    previous: &BTreeMap<String, u64>,
    current: &BTreeMap<String, u64>,
) -> BTreeSet<String> {
    previous
        .keys()
        .chain(current.keys())
        .filter(|name| previous.get(*name) != current.get(*name))
        .cloned()
        .collect()
}

impl Engine {
    /// An epoch may be published after the physical commit was already seen.
    /// Reconcile against the durable generations rather than decoding again.
    pub(super) fn refresh_tracked_storage_snapshot(&self) -> StorageBackendResult<bool> {
        if self.epochs.storage_cache_revisions.lock().is_none() {
            return Ok(false);
        }
        let Some(backend) = self.storage.backend.as_ref() else {
            return Ok(false);
        };
        let _statement = self.runtime.statement_gate.lock();
        let _refresh = self.epochs.external_commit_refresh.lock();
        if backend.in_transaction() {
            return Ok(false);
        }
        backend.begin_read_transaction()?;
        let result = self.refresh_pinned_transaction_snapshot();
        let cleanup = backend.rollback_transaction();
        match (result, cleanup) {
            (Ok(()), Ok(())) => Ok(true),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(error), Err(cleanup)) => Err(StorageBackendError::Other(format!(
                "cache refresh failed: {error}; snapshot cleanup failed: {cleanup}"
            ))),
        }
    }

    pub(super) fn refresh_tracked_pinned_snapshot(
        &self,
        catalog_epoch: u64,
        data_epoch: u64,
        registry_epoch: u64,
    ) -> StorageBackendResult<bool> {
        let Some(catalog) = self.storage.catalog.as_ref() else {
            return Ok(false);
        };
        let Some(current) = catalog.cache_revisions()? else {
            return Ok(false);
        };
        let previous = self.epochs.storage_cache_revisions.lock().clone();
        let catalog_changed = previous.as_ref().is_none_or(|previous| {
            previous.table_catalog != current.table_catalog
                || previous.storage_schema != current.storage_schema
        });
        let registries_changed = catalog_changed
            || previous
                .as_ref()
                .is_none_or(|previous| previous.registries != current.registries);
        if catalog_changed {
            self.clear_persistent_table_bindings_for_catalog_reload();
            self.reload_table_catalog(catalog_epoch)?;
            self.synchronize_partition_identity_watermarks()?;
        } else if let Some(previous) = previous.as_ref() {
            self.refresh_changed_table_caches(previous, &current)?;
        }
        if registries_changed {
            self.reload_catalog_registries(registry_epoch)?;
        } else if let Some(graphs) = current.graphs.as_ref() {
            let changed = self.refresh_graph_handles(
                catalog.as_ref(),
                previous
                    .as_ref()
                    .and_then(|previous| previous.graphs.as_ref()),
                graphs,
            )?;
            if !changed.is_empty() {
                // Graph namespaces and AGE label relations also appear in
                // regnamespace/regclass output, independently of SQL DDL.
                self.clear_regtype_output_cache();
            }
            self.refresh_changed_graph_path_indexes(catalog.as_ref(), &changed)?;
        }
        // Generations become observed only after every dependent cache was
        // restored successfully. An error leaves the snapshot eligible to retry.
        self.epochs
            .table_catalog
            .seen
            .store(catalog_epoch, Ordering::Release);
        self.epochs
            .table_data
            .seen
            .store(data_epoch, Ordering::Release);
        self.epochs
            .catalog_registry
            .seen
            .store(registry_epoch, Ordering::Release);
        if previous.as_ref() != Some(&current) {
            self.clear_sql_statement_cache();
            self.clear_bayesian_params_cache();
            self.invalidate_prepared_plans();
        }
        *self.epochs.storage_cache_revisions.lock() = Some(current);
        Ok(true)
    }

    fn refresh_changed_table_caches(
        &self,
        previous: &CatalogCacheRevisions,
        current: &CatalogCacheRevisions,
    ) -> StorageBackendResult<()> {
        let data = changed_names(&previous.table_data, &current.table_data);
        let statistics = changed_names(&previous.column_statistics, &current.column_statistics);
        let maintenance = changed_names(
            &previous.statistics_maintenance,
            &current.statistics_maintenance,
        );
        let names = data
            .iter()
            .chain(&statistics)
            .chain(&maintenance)
            .cloned()
            .collect::<BTreeSet<_>>();
        let catalog = self
            .storage
            .catalog
            .as_ref()
            .expect("tracked persistent catalog");
        let private_snapshot = self
            .storage
            .backend
            .as_ref()
            .map(|backend| backend.transaction_has_written())
            .transpose()?
            .unwrap_or(false);
        let mut partition_watermarks_changed = false;
        for name in names {
            let relation = uqa_storage::RelationIdentity::from_legacy_name(&name)
                .map_err(StorageBackendError::Other)?;
            let table = self.storage.tables.read().get(&relation).cloned();
            let Some(table) = table else {
                continue;
            };
            if table.persistence == uqa_sql::ast::RelationPersistence::Temporary {
                continue;
            }
            if data.contains(&name) {
                self.rebind_persistent_table_stores(&name, &table)?;
                self.refresh_table_next_id(&name, &table)?;
                table.doc_count_dirty.store(true, Ordering::Release);
                let hierarchy = table.hierarchy.read();
                partition_watermarks_changed |= (hierarchy.partition_spec.is_some()
                    || hierarchy.is_partition())
                    && table.columns.read().iter().any(|column| {
                        column
                            .auto_increment
                            .as_ref()
                            .is_some_and(uqa_sql::ast::AutoIncrement::is_legacy)
                    });
            }
            if statistics.contains(&name) {
                let load = || Self::load_column_stats_from_catalog(catalog.as_ref(), &name);
                // A private transaction may reuse a durable counter value
                // after rollback. Never publish its payload to sibling sessions.
                let stats = if private_snapshot {
                    std::sync::Arc::new(load()?)
                } else {
                    self.statistics.statistics_snapshots.load(
                        &name,
                        table.object_id(),
                        current
                            .column_statistics
                            .get(&name)
                            .copied()
                            .unwrap_or_default(),
                        load,
                    )?
                };
                table.column_stats.restore(&stats);
                table.column_stats_loaded.store(true, Ordering::Release);
            }
            let dirty = (table.column_stats.read().is_empty() && !table.columns.read().is_empty())
                || crate::statistics::MaintenanceState::load_for(
                    catalog.as_ref(),
                    &name,
                    table.object_id(),
                )?
                .invalidates_existing_statistics();
            table.column_stats_dirty.store(dirty, Ordering::Release);
        }
        if partition_watermarks_changed {
            self.synchronize_partition_identity_watermarks()?;
        }
        Ok(())
    }
}
