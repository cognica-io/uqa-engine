//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table-definition and durable-registry epoch synchronization.

use super::{Arc, BTreeMap, DeepModel, Engine, StorageBackendError, StorageBackendResult};

#[derive(Clone, Copy)]
struct CatalogVersions {
    table: u64,
    registry: u64,
    storage: Option<u64>,
}

impl Engine {
    pub(crate) fn table_catalog_metadata_fingerprint(
        table: &super::TableState,
    ) -> StorageBackendResult<Vec<u8>> {
        let vector_dimensions = table
            .vector_indexes
            .read()
            .iter()
            .map(|(field, index)| (field.clone(), index.dimensions()))
            .collect::<BTreeMap<_, _>>();
        let constraints = uqa_sql::ast::TableConstraintSet {
            checks: table.table_checks.read().clone(),
            foreign_keys: table.foreign_keys.read().clone(),
            key_constraints: table.key_constraints.read().clone(),
            persistence: table.persistence,
            on_commit: table.on_commit,
            hierarchy: table.hierarchy.read().clone(),
        };
        serde_json::to_vec(&(
            table.analyzer.read().clone(),
            table.fts_fields.read().clone(),
            vector_dimensions,
            table.columns.read().clone(),
            constraints,
        ))
        .map_err(|error| {
            StorageBackendError::Other(format!(
                "serialize fixed-transaction table catalog fingerprint: {error}"
            ))
        })
    }

    fn fixed_transaction_catalog_baseline(
        tables: &BTreeMap<uqa_storage::RelationIdentity, Arc<super::TableState>>,
    ) -> StorageBackendResult<crate::FixedTransactionCatalogBaseline> {
        let mut baseline = BTreeMap::new();
        for (relation, table) in tables {
            if table.persistence == uqa_sql::ast::RelationPersistence::Temporary {
                continue;
            }
            baseline.insert(
                table.storage_generation(),
                (
                    relation.clone(),
                    Self::table_catalog_metadata_fingerprint(table)?,
                ),
            );
        }
        Ok(baseline)
    }

    pub(crate) fn capture_fixed_transaction_catalog_baseline(
        &self,
    ) -> Result<crate::FixedTransactionCatalogBaseline, crate::SQLError> {
        Self::fixed_transaction_catalog_baseline(&self.storage.tables.read()).map_err(|error| {
            crate::SQLError::Internal(format!(
                "capture fixed-transaction table catalog baseline: {error}"
            ))
        })
    }

    fn merged_latest_fixed_snapshot_table_catalog(
        &self,
        latest: &Engine,
        current: &BTreeMap<uqa_storage::RelationIdentity, Arc<super::TableState>>,
    ) -> StorageBackendResult<(
        BTreeMap<uqa_storage::RelationIdentity, Arc<super::TableState>>,
        crate::FixedTransactionCatalogBaseline,
    )> {
        let baseline = self
            .session
            .transactions
            .lock()
            .first()
            .and_then(|frame| frame.fixed_catalog_baseline.clone())
            .unwrap_or_default();
        let latest_tables = latest.storage.tables.read().clone();
        let latest_baseline = Self::fixed_transaction_catalog_baseline(&latest_tables)?;
        let current_generations = current
            .values()
            .map(|table| table.storage_generation())
            .collect::<std::collections::BTreeSet<_>>();
        let suppressed_generations = baseline
            .keys()
            .filter(|generation| !current_generations.contains(*generation))
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        let mut local = Vec::new();
        for (relation, table) in current {
            let generation = table.storage_generation();
            let current_fingerprint = Self::table_catalog_metadata_fingerprint(table)?;
            let transaction_local = table.persistence
                == uqa_sql::ast::RelationPersistence::Temporary
                || baseline.get(&generation).is_none_or(
                    |(committed_relation, committed_fingerprint)| {
                        committed_relation != relation
                            || committed_fingerprint != &current_fingerprint
                    },
                );
            if transaction_local {
                local.push((relation.clone(), Arc::clone(table)));
            }
        }
        let mut merged = latest_tables
            .into_iter()
            .filter(|(_, table)| !suppressed_generations.contains(&table.storage_generation()))
            .collect::<BTreeMap<_, _>>();
        for (relation, table) in local {
            let generation = table.storage_generation();
            merged.retain(|_, candidate| candidate.storage_generation() != generation);
            merged.insert(relation, table);
        }
        Ok((merged, latest_baseline))
    }

    fn published_catalog_versions(&self) -> StorageBackendResult<CatalogVersions> {
        let storage = match self.storage.backend.as_ref() {
            Some(backend) if backend.change_version_monitor_is_nonblocking()? => {
                backend.change_version()?
            }
            _ => None,
        };
        Ok(CatalogVersions {
            table: self
                .epochs
                .table_catalog
                .published
                .load(std::sync::atomic::Ordering::Acquire),
            registry: self
                .epochs
                .catalog_registry
                .published
                .load(std::sync::atomic::Ordering::Acquire),
            storage,
        })
    }

    fn swap_seen_catalog_versions(&self, versions: CatalogVersions) -> CatalogVersions {
        CatalogVersions {
            table: self
                .epochs
                .table_catalog
                .seen
                .swap(versions.table, std::sync::atomic::Ordering::AcqRel),
            registry: self
                .epochs
                .catalog_registry
                .seen
                .swap(versions.registry, std::sync::atomic::Ordering::AcqRel),
            storage: versions.storage.map(|version| {
                self.epochs
                    .seen_storage_change_version
                    .swap(version, std::sync::atomic::Ordering::AcqRel)
            }),
        }
    }

    fn store_seen_catalog_versions(&self, versions: CatalogVersions) {
        self.epochs
            .table_catalog
            .seen
            .store(versions.table, std::sync::atomic::Ordering::Release);
        self.epochs
            .catalog_registry
            .seen
            .store(versions.registry, std::sync::atomic::Ordering::Release);
        if let Some(version) = versions.storage {
            self.epochs
                .seen_storage_change_version
                .store(version, std::sync::atomic::Ordering::Release);
        }
    }

    fn install_latest_fixed_transaction_catalogs(
        &self,
        latest: &Engine,
        target_versions: CatalogVersions,
    ) -> StorageBackendResult<()> {
        let previous_tables = self.storage.tables.read().clone();
        let (merged_tables, latest_baseline) =
            self.merged_latest_fixed_snapshot_table_catalog(latest, &previous_tables)?;
        let previous_durable = self.durable.snapshot();
        let temporary_views = self
            .durable
            .views
            .read()
            .iter()
            .filter(|(_, view)| view.persistence == uqa_sql::ast::RelationPersistence::Temporary)
            .map(|(relation, view)| (relation.clone(), view.clone()))
            .collect::<BTreeMap<_, _>>();
        let temporary_sequence_persistence = self
            .durable
            .sequence_persistence
            .read()
            .iter()
            .filter(|(_, persistence)| {
                **persistence == uqa_sql::ast::RelationPersistence::Temporary
            })
            .map(|(relation, persistence)| (relation.clone(), *persistence))
            .collect::<BTreeMap<_, _>>();
        let temporary_sequences = self
            .durable
            .sequences
            .read()
            .iter()
            .filter(|(relation, _)| temporary_sequence_persistence.contains_key(*relation))
            .map(|(relation, state)| (relation.clone(), *state))
            .collect::<BTreeMap<_, _>>();
        let temporary_sequence_object_ids = self
            .durable
            .sequence_object_ids
            .read()
            .iter()
            .filter(|(relation, _)| temporary_sequence_persistence.contains_key(*relation))
            .map(|(relation, object_id)| (relation.clone(), *object_id))
            .collect::<BTreeMap<_, _>>();
        self.durable.restore(&latest.durable.snapshot());
        self.durable.views.write().extend(temporary_views);
        self.durable.sequences.write().extend(temporary_sequences);
        self.durable
            .sequence_object_ids
            .write()
            .extend(temporary_sequence_object_ids);
        self.durable
            .sequence_persistence
            .write()
            .extend(temporary_sequence_persistence);
        let previous_versions = self.swap_seen_catalog_versions(target_versions);
        let rollback = || {
            *self.storage.tables.write() = previous_tables.clone();
            self.durable.restore(&previous_durable);
            self.store_seen_catalog_versions(previous_versions);
            self.clear_regtype_output_cache();
            self.clear_bayesian_params_cache();
            self.clear_sql_statement_cache();
        };
        for (relation, table) in &merged_tables {
            let already_bound = previous_tables
                .values()
                .any(|previous| Arc::ptr_eq(previous, table));
            if table.persistence == uqa_sql::ast::RelationPersistence::Temporary || already_bound {
                continue;
            }
            if let Err(error) =
                self.rebind_persistent_table_stores(&relation.qualified_name(), table)
            {
                rollback();
                return Err(error);
            }
        }
        *self.storage.tables.write() = merged_tables;
        self.clear_regtype_output_cache();
        self.clear_bayesian_params_cache();
        self.clear_sql_statement_cache();
        if let Err(error) = self.rebind_prepared_plans() {
            rollback();
            return Err(StorageBackendError::Other(format!(
                "re-optimize prepared plans after fixed-snapshot catalog refresh: {error}"
            )));
        }
        if let Some(frame) = self.session.transactions.lock().first_mut() {
            frame.fixed_catalog_baseline = Some(latest_baseline);
        }
        Ok(())
    }

    fn synchronize_fixed_transaction_catalogs(&self) -> StorageBackendResult<bool> {
        let Some(transactions) = self.session.transactions.try_lock() else {
            return Ok(false);
        };
        let fixed_snapshot_set = transactions
            .first()
            .is_some_and(|frame| frame.fixed_snapshot.is_some());
        drop(transactions);
        if !fixed_snapshot_set {
            return Ok(false);
        }
        let target_versions = self.published_catalog_versions()?;
        let catalog_epochs_current = self
            .epochs
            .table_catalog
            .seen
            .load(std::sync::atomic::Ordering::Acquire)
            == target_versions.table
            && self
                .epochs
                .catalog_registry
                .seen
                .load(std::sync::atomic::Ordering::Acquire)
                == target_versions.registry;
        let storage_current = target_versions.storage.is_none_or(|version| {
            self.epochs
                .seen_storage_change_version
                .load(std::sync::atomic::Ordering::Acquire)
                == version
        });
        if catalog_epochs_current && storage_current {
            return Ok(true);
        }
        let in_process_data_commit = self
            .epochs
            .table_data
            .seen
            .load(std::sync::atomic::Ordering::Acquire)
            != self
                .epochs
                .table_data
                .published
                .load(std::sync::atomic::Ordering::Acquire);
        if catalog_epochs_current && in_process_data_commit {
            self.store_seen_catalog_versions(target_versions);
            return Ok(true);
        }

        let latest = self.new_session()?;
        self.install_latest_fixed_transaction_catalogs(&latest, target_versions)?;
        Ok(true)
    }

    /// Mark a durable non-table registry change. Explicit transactions keep
    /// the generation private until their outer COMMIT; autocommit operations
    /// publish immediately.
    pub(crate) fn note_catalog_registry_changed(&self) {
        self.mutation_coordinator().note_catalog_registry_changed();
    }

    pub(crate) fn publish_catalog_registry_changes(&self) {
        self.mutation_coordinator()
            .publish_catalog_registry_changes();
    }

    /// Rebind this session's physical table handles when another session has
    /// changed the durable table catalog. Logical definitions come from the
    /// catalog; document/FTS/vector handles always come from `self.storage.backend`.
    pub(crate) fn synchronize_table_catalog(&self) -> StorageBackendResult<()> {
        // An explicit transaction owns a pinned storage snapshot. Never consume
        // a sibling's newer in-process epoch while reading that older
        // snapshot; the next call after COMMIT/ROLLBACK will perform the
        // refresh. Outer BEGIN uses `refresh_pinned_transaction_snapshot`
        // directly after acquiring its snapshot.
        if self
            .storage
            .backend
            .as_ref()
            .is_some_and(|backend| backend.in_transaction())
        {
            self.synchronize_fixed_transaction_catalogs()?;
            return Ok(());
        }
        self.synchronize_external_commits()?;
        let target_epoch = self
            .epochs
            .table_catalog
            .published
            .load(std::sync::atomic::Ordering::Acquire);
        if self
            .epochs
            .table_catalog
            .seen
            .load(std::sync::atomic::Ordering::Acquire)
            == target_epoch
        {
            return Ok(());
        }

        let _refresh = self.epochs.table_catalog.refresh.lock();
        let target_epoch = self
            .epochs
            .table_catalog
            .published
            .load(std::sync::atomic::Ordering::Acquire);
        if self
            .epochs
            .table_catalog
            .seen
            .load(std::sync::atomic::Ordering::Acquire)
            == target_epoch
        {
            return Ok(());
        }
        self.reload_table_catalog(target_epoch)
    }

    pub(crate) fn reload_table_catalog_after_rollback(&self) -> StorageBackendResult<()> {
        self.clear_persistent_table_bindings_for_catalog_reload();
        let target_epoch = self
            .epochs
            .table_catalog
            .published
            .load(std::sync::atomic::Ordering::Acquire);
        self.reload_table_catalog(target_epoch)?;
        self.epochs
            .table_catalog
            .dirty
            .store(false, std::sync::atomic::Ordering::Release);
        Ok(())
    }

    /// A composite catalog refresh reloads these maps from durable rows after
    /// rebuilding table handles. Clear them first so an uncommitted/rolled-
    /// back analyzer or vector-index binding cannot be applied to the fresh
    /// stores during the intermediate table reload.
    pub(super) fn clear_persistent_table_bindings_for_catalog_reload(&self) {
        self.durable.table_field_analyzers.write().clear();
        self.durable.catalog_indexes.write().clear();
    }

    pub(super) fn reload_table_catalog(&self, target_epoch: u64) -> StorageBackendResult<()> {
        self.clear_regtype_output_cache();
        let previous_epoch = self
            .epochs
            .table_catalog
            .seen
            .load(std::sync::atomic::Ordering::Acquire);
        let Some(catalog) = self.storage.catalog.as_ref() else {
            self.epochs
                .table_catalog
                .seen
                .store(target_epoch, std::sync::atomic::Ordering::Release);
            return Ok(());
        };
        let Some(backend) = self.storage.backend.as_ref() else {
            return Err(StorageBackendError::Other(
                "persistent catalog has no matching storage backend".into(),
            ));
        };

        let existing_lifetimes = self
            .storage
            .tables
            .read()
            .iter()
            .map(|(relation, table)| {
                (
                    relation.clone(),
                    (table.lifecycle_id(), table.storage_generation()),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut rebound = BTreeMap::new();
        for schema in catalog.load_tables()? {
            let relation = schema.relation.clone();
            let table = Self::load_session_table(catalog.as_ref(), backend.as_ref(), schema)?;
            if let Some((lifecycle_id, storage_generation)) = existing_lifetimes.get(&relation) {
                if *storage_generation == table.storage_generation() {
                    table
                        .lifecycle_id
                        .store(*lifecycle_id, std::sync::atomic::Ordering::Release);
                }
            }
            rebound.insert(relation, table);
        }
        rebound.extend(
            self.storage
                .tables
                .read()
                .iter()
                .filter(|(_, table)| {
                    table.persistence == uqa_sql::ast::RelationPersistence::Temporary
                })
                .map(|(relation, table)| (relation.clone(), table.clone())),
        );
        *self.storage.tables.write() = rebound;

        // Restore per-field analyzers and IVF/HNSW bindings on the newly
        // created session-local stores. These registries are logical state
        // shared by sibling sessions.
        let tables = self
            .storage
            .tables
            .read()
            .iter()
            .map(|(name, table)| (name.clone(), table.clone()))
            .collect::<Vec<_>>();
        for (name, table) in tables {
            if table.persistence == uqa_sql::ast::RelationPersistence::Temporary {
                continue;
            }
            self.rebind_persistent_table_stores(&name.qualified_name(), &table)?;
        }
        self.epochs
            .table_catalog
            .seen
            .store(target_epoch, std::sync::atomic::Ordering::Release);
        self.clear_sql_statement_cache();
        if let Err(error) = self.rebind_prepared_plans() {
            self.epochs
                .table_catalog
                .seen
                .store(previous_epoch, std::sync::atomic::Ordering::Release);
            return Err(StorageBackendError::Other(format!(
                "re-optimize prepared plans after table catalog refresh: {error}"
            )));
        }
        Ok(())
    }

    /// Refresh session-local durable registry caches after a sibling commits.
    /// The backend supplies snapshot isolation, so a reader
    /// never observes another session's uncommitted registry changes.
    pub(crate) fn synchronize_catalog_registries(&self) -> StorageBackendResult<()> {
        if self
            .storage
            .backend
            .as_ref()
            .is_some_and(|backend| backend.in_transaction())
        {
            self.synchronize_fixed_transaction_catalogs()?;
            return Ok(());
        }
        self.synchronize_external_commits()?;
        let target_epoch = self
            .epochs
            .catalog_registry
            .published
            .load(std::sync::atomic::Ordering::Acquire);
        if self
            .epochs
            .catalog_registry
            .seen
            .load(std::sync::atomic::Ordering::Acquire)
            == target_epoch
        {
            return Ok(());
        }
        let _refresh = self.epochs.catalog_registry.refresh.lock();
        let target_epoch = self
            .epochs
            .catalog_registry
            .published
            .load(std::sync::atomic::Ordering::Acquire);
        if self
            .epochs
            .catalog_registry
            .seen
            .load(std::sync::atomic::Ordering::Acquire)
            == target_epoch
        {
            return Ok(());
        }
        self.reload_catalog_registries(target_epoch)
    }

    pub(crate) fn reload_catalog_registries_after_rollback(&self) -> StorageBackendResult<()> {
        let target_epoch = self
            .epochs
            .catalog_registry
            .published
            .load(std::sync::atomic::Ordering::Acquire);
        self.reload_catalog_registries(target_epoch)?;
        self.epochs
            .catalog_registry
            .dirty
            .store(false, std::sync::atomic::Ordering::Release);
        Ok(())
    }

    pub(super) fn reload_catalog_registries(&self, target_epoch: u64) -> StorageBackendResult<()> {
        self.clear_regtype_output_cache();
        self.clear_bayesian_params_cache();
        let previous_epoch = self
            .epochs
            .catalog_registry
            .seen
            .load(std::sync::atomic::Ordering::Acquire);
        let Some(catalog) = self.storage.catalog.as_ref() else {
            self.epochs
                .catalog_registry
                .seen
                .store(target_epoch, std::sync::atomic::Ordering::Release);
            return Ok(());
        };

        let temporary_views = self
            .durable
            .views
            .read()
            .iter()
            .filter(|(_, view)| view.persistence == uqa_sql::ast::RelationPersistence::Temporary)
            .map(|(relation, view)| (relation.clone(), view.clone()))
            .collect::<BTreeMap<_, _>>();
        self.durable.graphs.write().clear();
        *self.durable.views.write() = temporary_views;
        self.durable.catalog_indexes.write().clear();
        self.durable.schemas.write().clear();
        self.durable.path_indexes.write().clear();
        self.durable.named_analyzers.write().clear();
        self.durable.table_field_analyzers.write().clear();
        self.durable.foreign_servers.write().clear();
        self.durable.foreign_tables.write().clear();
        self.durable.sql_user_functions.write().clear();
        self.durable.models.write().clear();
        self.durable.scoring_params.write().clear();

        self.restore_schemas_from_catalog(catalog.as_ref())?;
        self.restore_graphs_from_catalog(catalog.as_ref())?;
        self.restore_engine_registries_from_catalog(catalog.as_ref())?;
        for (name, json) in catalog.load_models()? {
            self.durable
                .models
                .write()
                .insert(name, serde_json::from_str::<DeepModel>(&json)?);
        }
        for (name, json) in catalog.load_all_scoring_params()? {
            self.durable.scoring_params.write().insert(name, json);
        }
        // Registry restoration can remove a table-field analyzer or replace a
        // vector/index binding. Recreate every persistent store after the
        // registry maps hold the durable snapshot; otherwise a rolled-back
        // analyzer that was applied during the preceding table reload can
        // survive in the physical session handle even though its catalog row
        // is gone.
        let tables = self
            .storage
            .tables
            .read()
            .iter()
            .map(|(relation, table)| (relation.qualified_name(), table.clone()))
            .collect::<Vec<_>>();
        for (name, table) in tables {
            if table.persistence == uqa_sql::ast::RelationPersistence::Temporary {
                continue;
            }
            self.rebind_persistent_table_stores(&name, &table)?;
        }
        self.epochs
            .catalog_registry
            .seen
            .store(target_epoch, std::sync::atomic::Ordering::Release);
        self.clear_sql_statement_cache();
        if let Err(error) = self.rebind_prepared_plans() {
            self.epochs
                .catalog_registry
                .seen
                .store(previous_epoch, std::sync::atomic::Ordering::Release);
            return Err(StorageBackendError::Other(format!(
                "re-optimize prepared plans after catalog registry refresh: {error}"
            )));
        }
        Ok(())
    }
}
