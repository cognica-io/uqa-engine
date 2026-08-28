//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cross-session table-data epochs and physical cache refresh.

use super::{Engine, StorageBackendError, StorageBackendResult};

impl Engine {
    /// Publish a committed logical table-definition change to sibling
    /// sessions. Their physical stores are rebuilt lazily from their own
    /// session-bound backend on the next table lookup.
    pub(crate) fn note_table_catalog_changed(&self) {
        self.clear_regtype_output_cache();
        self.clear_bayesian_params_cache();
        if !self.session.transactions.lock().is_empty() {
            self.epochs
                .table_catalog
                .dirty
                .store(true, std::sync::atomic::Ordering::Release);
            return;
        }
        self.publish_table_catalog_changes();
    }

    pub(crate) fn publish_table_catalog_changes(&self) {
        self.clear_bayesian_params_cache();
        self.epochs
            .table_catalog
            .published
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        self.epochs
            .table_catalog
            .dirty
            .store(false, std::sync::atomic::Ordering::Release);
        // The writer's physical stores are current, but cached optimized and
        // prepared plans may retain a removed access path or old schema.
        // Leave `seen_table_catalog_epoch` behind so its next statement also
        // crosses the same reload/re-optimization boundary as siblings.
        self.clear_sql_statement_cache();
    }

    /// Mark table contents changed in this session. The generation is only
    /// published after the outer storage transaction commits, so sibling
    /// sessions cannot invalidate and rebuild against uncommitted data.
    pub(crate) fn note_table_data_changed(&self) {
        self.clear_bayesian_params_cache();
        self.clear_sql_statement_cache();
        // Rollback restoration replaces snapshots directly and never enters
        // this ordinary mutation hook. Therefore contention is not evidence
        // of an active transaction: wait for the stack and inspect its state.
        // This prevents an unrelated session thread from turning an
        // autocommit write into an unpublished dirty generation.
        let transaction_active = !self.session.transactions.lock().is_empty();
        if transaction_active {
            self.epochs
                .table_data
                .dirty
                .store(true, std::sync::atomic::Ordering::Release);
            return;
        }
        self.publish_table_data_changes();
    }

    pub(crate) fn publish_table_data_changes(&self) {
        self.clear_bayesian_params_cache();
        self.epochs
            .table_data
            .published
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        // Keep this session's observed generation behind too. Its ordinary
        // write caches were updated incrementally, but prepared/optimized
        // plans and every derived store must cross the same refresh boundary
        // as sibling sessions before the next statement.
        self.epochs
            .table_data
            .dirty
            .store(false, std::sync::atomic::Ordering::Release);
        self.clear_sql_statement_cache();
    }

    /// Refresh every session-local dependency of committed table contents.
    /// Calls made inside an already-pinned storage transaction intentionally
    /// defer the refresh: that transaction must keep using its original
    /// snapshot and will observe the new generation after it finishes.
    pub(crate) fn synchronize_table_data(&self) -> StorageBackendResult<()> {
        if self
            .storage
            .backend
            .as_ref()
            .is_some_and(|backend| backend.in_transaction())
        {
            return Ok(());
        }
        self.synchronize_external_commits()?;
        self.refresh_table_data_cache(false)
    }

    /// Detect commits made by independently opened engines or other
    /// processes. In-process Arc epochs only coordinate sessions derived via
    /// `new_session`; a backend commit generation closes the same visibility
    /// gap for every other writer when the backend exposes one.
    pub(super) fn synchronize_external_commits(&self) -> StorageBackendResult<()> {
        let Some(backend) = self.storage.backend.as_ref() else {
            return Ok(());
        };
        if backend.in_transaction() {
            return Ok(());
        }
        let Some(version) = backend.change_version()? else {
            return Ok(());
        };
        if self
            .epochs
            .seen_storage_change_version
            .load(std::sync::atomic::Ordering::Acquire)
            == version
        {
            return Ok(());
        }

        let _statement = self.runtime.statement_gate.lock();
        let _refresh = self.epochs.external_commit_refresh.lock();
        if backend.in_transaction() {
            return Ok(());
        }
        let Some(version) = backend.change_version()? else {
            return Ok(());
        };
        if self
            .epochs
            .seen_storage_change_version
            .load(std::sync::atomic::Ordering::Acquire)
            == version
        {
            return Ok(());
        }

        // Mark this version while rebuilding so catalog restore helpers that
        // resolve a table cannot recursively enter the same non-reentrant
        // refresh lock. Restore the old marker on failure; a commit racing the
        // rebuild will advance the monitor again and be handled next time.
        let previous_version = self
            .epochs
            .seen_storage_change_version
            .swap(version, std::sync::atomic::Ordering::AcqRel);
        let refresh_result = (|| {
            self.clear_persistent_table_bindings_for_catalog_reload();
            let table_catalog_epoch = self
                .epochs
                .table_catalog
                .published
                .load(std::sync::atomic::Ordering::Acquire);
            self.reload_table_catalog(table_catalog_epoch)?;
            self.refresh_table_data_cache(true)?;
            let catalog_registry_epoch = self
                .epochs
                .catalog_registry
                .published
                .load(std::sync::atomic::Ordering::Acquire);
            self.reload_catalog_registries(catalog_registry_epoch)
        })();
        if refresh_result.is_err() {
            self.epochs
                .seen_storage_change_version
                .store(previous_version, std::sync::atomic::Ordering::Release);
        }
        refresh_result
    }

    pub(super) fn refresh_table_data_cache(&self, force: bool) -> StorageBackendResult<()> {
        let target_epoch = self
            .epochs
            .table_data
            .published
            .load(std::sync::atomic::Ordering::Acquire);
        if !force
            && self
                .epochs
                .table_data
                .seen
                .load(std::sync::atomic::Ordering::Acquire)
                == target_epoch
        {
            return Ok(());
        }

        let _refresh = self.epochs.table_data.refresh.lock();
        let target_epoch = self
            .epochs
            .table_data
            .published
            .load(std::sync::atomic::Ordering::Acquire);
        let previous_epoch = self
            .epochs
            .table_data
            .seen
            .load(std::sync::atomic::Ordering::Acquire);
        if !force && previous_epoch == target_epoch {
            return Ok(());
        }
        self.clear_bayesian_params_cache();

        let tables = self
            .storage
            .tables
            .read()
            .iter()
            .map(|(name, table)| (name.clone(), table.clone()))
            .collect::<Vec<_>>();
        for (name, table) in tables {
            let name = name.qualified_name();
            let temporary = table.persistence == uqa_sql::ast::RelationPersistence::Temporary;
            if self.storage.backend.is_some() && !temporary {
                self.rebind_persistent_table_stores(&name, &table)?;
                self.refresh_table_next_id(&name, &table)?;
            } else {
                Self::value_indexes_clear(&table);
            }
            table
                .doc_count_dirty
                .store(true, std::sync::atomic::Ordering::Release);
            if temporary {
                table
                    .column_stats_dirty
                    .store(true, std::sync::atomic::Ordering::Release);
            } else if let Some(catalog) = self.storage.catalog.as_ref() {
                let stats = Self::load_column_stats_from_catalog(catalog.as_ref(), &name)?;
                let stats_dirty = stats.is_empty() && !table.columns.read().is_empty();
                *table.column_stats.write() = stats;
                table
                    .column_stats_loaded
                    .store(true, std::sync::atomic::Ordering::Release);
                table
                    .column_stats_dirty
                    .store(stats_dirty, std::sync::atomic::Ordering::Release);
            } else {
                table
                    .column_stats_dirty
                    .store(true, std::sync::atomic::Ordering::Release);
            }
        }
        self.synchronize_partition_identity_watermarks()?;
        self.clear_sql_statement_cache();
        // Set the generation before rebinding prepared plans so optimizer
        // statistics can resolve tables without recursively refreshing.
        self.epochs
            .table_data
            .seen
            .store(target_epoch, std::sync::atomic::Ordering::Release);
        if let Err(error) = self.rebind_prepared_plans() {
            self.epochs
                .table_data
                .seen
                .store(previous_epoch, std::sync::atomic::Ordering::Release);
            return Err(StorageBackendError::Other(format!(
                "re-optimize prepared plans after table data refresh: {error}"
            )));
        }
        Ok(())
    }

    /// Bring every session-local cache onto the outer transaction's pinned
    /// database snapshot. A stable backend change version closes the gap
    /// between a physical commit and publication of the matching in-process
    /// epochs, while allowing unchanged statements to retain their caches.
    pub(crate) fn refresh_pinned_transaction_snapshot(&self) -> StorageBackendResult<()> {
        let (storage_snapshot_unchanged, stable_storage_version) =
            if let Some(backend) = self.storage.backend.as_ref() {
                if backend.change_version_monitor_is_nonblocking()? {
                    let before = backend.change_version()?;
                    backend.pin_transaction_snapshot()?;
                    let after = backend.change_version()?;
                    let stable = before == after;
                    (
                        stable
                            && after.is_some_and(|version| {
                                self.epochs
                                    .seen_storage_change_version
                                    .load(std::sync::atomic::Ordering::Acquire)
                                    == version
                            }),
                        stable.then_some(after).flatten(),
                    )
                } else {
                    // A backend may own a whole-file exclusive lock. Pin and
                    // refresh through the session itself because an independent
                    // monitor could wait on a lock held by this same session.
                    backend.pin_transaction_snapshot()?;
                    (false, None)
                }
            } else {
                (true, None)
            };
        let table_catalog_epoch = self
            .epochs
            .table_catalog
            .published
            .load(std::sync::atomic::Ordering::Acquire);
        let table_data_epoch = self
            .epochs
            .table_data
            .published
            .load(std::sync::atomic::Ordering::Acquire);
        let catalog_registry_epoch = self
            .epochs
            .catalog_registry
            .published
            .load(std::sync::atomic::Ordering::Acquire);
        if storage_snapshot_unchanged
            && self
                .epochs
                .table_catalog
                .seen
                .load(std::sync::atomic::Ordering::Acquire)
                == table_catalog_epoch
            && self
                .epochs
                .table_data
                .seen
                .load(std::sync::atomic::Ordering::Acquire)
                == table_data_epoch
            && self
                .epochs
                .catalog_registry
                .seen
                .load(std::sync::atomic::Ordering::Acquire)
                == catalog_registry_epoch
        {
            return Ok(());
        }

        self.clear_persistent_table_bindings_for_catalog_reload();
        self.reload_table_catalog(table_catalog_epoch)?;
        self.refresh_table_data_cache(true)?;
        self.reload_catalog_registries(catalog_registry_epoch)?;
        if let Some(version) = stable_storage_version {
            self.epochs
                .seen_storage_change_version
                .store(version, std::sync::atomic::Ordering::Release);
        }
        Ok(())
    }
}
