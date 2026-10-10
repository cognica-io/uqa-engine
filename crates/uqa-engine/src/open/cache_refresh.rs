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

fn observed_private_data(
    name: &str,
    current: &CatalogCacheRevisions,
    previous_view: Option<&uqa_storage::key_value::KeyValueReadRevision>,
) -> bool {
    current
        .table_data
        .get(name)
        .is_some_and(|value| CatalogCacheRevisions::is_private_generation(*value))
        && current
            .table_data_commits
            .get(name)
            .is_some_and(|(database, sequence)| {
                previous_view.is_some_and(|view| view.includes_commit(*database, *sequence))
            })
}

impl Engine {
    /// After this session's own data commit, keep its table caches when that commit is the only one since the view they reflect. Its writes already maintained them, so the next statement need not rebuild them as it must after another writer. Any other commit, or a catalog, registry, graph or schema revision change, leaves the observed state behind for the ordinary refresh. A catalog that reports no cache revisions, before as after, relies on the caller, which adopts only a commit whose dirty state shows data changes alone. A failure to observe the committed view has the same effect as another commit: the next statement's refresh reads it again and reports any persistent error.
    pub(crate) fn adopt_own_commit_revisions(&self) {
        let Some(backend) = self.storage.backend.as_ref() else {
            return;
        };
        let Some(catalog) = self.storage.catalog.as_ref() else {
            return;
        };
        let previous_view = self.epochs.seen_storage_read_view.lock().clone();
        let Some(previous_view) = previous_view else {
            return;
        };
        if backend.in_transaction() {
            return;
        }
        // Observed before the view: a sibling that publishes later remains unobserved.
        let data_epoch = self.epochs.table_data.published.load(Ordering::Acquire);
        if let Ok(Some(committed)) = backend.committed_data_revision() {
            if committed.revision.follows_by_one_commit(&previous_view) {
                // The provider certified that this exact commit preserved definitions. Retain the older data-generation baseline: a later foreign commit can then cause an extra refresh, but cannot hide a changed table. The receipt is this session's view, never a claim to have observed a later writer.
                self.adopt_committed_view(
                    committed.revision,
                    Some(committed.change_version),
                    data_epoch,
                );
                return;
            }
        }
        let previous = self.epochs.storage_cache_revisions.lock().clone();
        // A versioned session's pinned view reports its own change version, which costs no further read of the latest commit. Any other session asks the monitor first: inside its read transaction the monitor could wait behind a writer that waits for this reader.
        let pinned_version = backend.transaction_model().is_versioned();
        let before = if pinned_version {
            None
        } else {
            match backend.change_version() {
                Ok(version) => Some(version),
                Err(_) => return,
            }
        };
        if backend.begin_read_transaction().is_err() {
            return;
        }
        let observed = before
            .map_or_else(|| backend.change_version(), Ok)
            .and_then(|version| {
                Ok((
                    version,
                    backend.read_view_revision()?,
                    catalog.cache_revisions()?,
                ))
            });
        if backend.rollback_transaction().is_err() {
            return;
        }
        let Ok((version, Some(view), current)) = observed else {
            return;
        };
        if !view.follows_by_one_commit(&previous_view) {
            return;
        }
        match (&previous, &current) {
            (Some(previous), Some(current)) => {
                if current.table_catalog != previous.table_catalog
                    || current.registries != previous.registries
                    || current.graphs != previous.graphs
                    || current.storage_schema != previous.storage_schema
                {
                    return;
                }
            }
            (None, None) => {}
            _ => return,
        }
        *self.epochs.storage_cache_revisions.lock() = current;
        self.adopt_committed_view(view, version, data_epoch);
    }

    fn adopt_committed_view(
        &self,
        view: uqa_storage::key_value::KeyValueReadRevision,
        version: Option<u64>,
        data_epoch: u64,
    ) {
        *self.epochs.seen_storage_read_view.lock() = Some(view);
        if let Some(version) = version {
            self.epochs
                .seen_storage_change_version
                .store(version, Ordering::Release);
        }
        self.epochs
            .table_data
            .seen
            .store(data_epoch, Ordering::Release);
        self.clear_bayesian_params_cache();
        self.invalidate_prepared_plans();
    }

    /// An epoch may be published after the physical commit was already seen.
    /// Reconcile against the durable generations rather than decoding again.
    pub(super) fn refresh_tracked_storage_snapshot(&self) -> StorageBackendResult<bool> {
        if self.epochs.storage_cache_revisions.lock().is_none() {
            return Ok(false);
        }
        let Some(backend) = self.storage.backend.as_ref() else {
            return Ok(false);
        };
        let _statement = self.lock_statement_gate();
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
        committed_unchanged: bool,
        previous_view: Option<&uqa_storage::key_value::KeyValueReadRevision>,
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
        // Physical indexes and analyzer bindings belong to the same committed revision. Restore changed registries before reopening data stores, so an old cached binding never meets newly committed occurrence metadata.
        if !catalog_changed {
            if let Some(previous) = previous.as_ref() {
                self.refresh_changed_table_caches(
                    previous,
                    &current,
                    committed_unchanged,
                    previous_view,
                )?;
            }
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
            if catalog_changed || registries_changed {
                self.clear_sql_statement_cache();
            } else {
                self.invalidate_optimized_sql_plans();
            }
            self.clear_bayesian_params_cache();
            self.invalidate_prepared_plans();
        }
        *self.epochs.storage_cache_revisions.lock() = Some(current);
        Ok(true)
    }

    /// `committed_unchanged` reports that the committed state is the one the caches were last refreshed from, so a private generation is this transaction's own change, which its writes already applied to the caches. A rollback restores the committed generation, which differs from the private one and refreshes the table.
    fn refresh_changed_table_caches(
        &self,
        previous: &CatalogCacheRevisions,
        current: &CatalogCacheRevisions,
        committed_unchanged: bool,
        previous_view: Option<&uqa_storage::key_value::KeyValueReadRevision>,
    ) -> StorageBackendResult<()> {
        let foreign = |previous: &BTreeMap<String, u64>, current: &BTreeMap<String, u64>| {
            let mut names = changed_names(previous, current);
            if committed_unchanged {
                names.retain(|name| {
                    !current.get(name).is_some_and(|generation| {
                        CatalogCacheRevisions::is_private_generation(*generation)
                    })
                });
            }
            names
        };
        let mut data = foreign(&previous.table_data, &current.table_data);
        // Own commit adoption can leave the generation baseline behind. Compare
        // the table's actual last committed change with the view the caches
        // reflect, rather than mistaking our own older commit for a peer write.
        data.retain(|name| !observed_private_data(name, current, previous_view));
        let statistics = foreign(&previous.column_statistics, &current.column_statistics);
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
