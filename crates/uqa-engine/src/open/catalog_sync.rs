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

#[derive(Clone, Copy)]
enum PreparedInvalidation {
    Execution,
    Analysis,
}

impl PreparedInvalidation {
    fn apply(self, engine: &Engine) {
        match self {
            Self::Execution => engine.invalidate_prepared_plans(),
            Self::Analysis => engine.invalidate_prepared_analysis(),
        }
    }
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
            columns_declared: Some(*table.columns_declared.read()),
            checks: table.table_checks.read().clone(),
            foreign_keys: table.foreign_keys.read().clone(),
            key_constraints: table.key_constraints.read().clone(),
            persistence: table.persistence,
            on_commit: table.on_commit,
            hierarchy: table.hierarchy.read().clone(),
            catalog_oids: table.recorded_catalog_oids(),
        };
        let security = table.security();
        serde_json::to_vec(&(
            security.role_owner,
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
        for (relation, table) in &merged {
            if let Some(previous) = current
                .get(relation)
                .filter(|previous| previous.object_id == table.object_id)
            {
                let security = uqa_execution::catalog::security::relation_authority::merge_private(
                    self.storage.catalog.as_deref(),
                    relation,
                    &previous.security(),
                    table.security(),
                )?;
                *table.security.write() = security;
            }
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

    fn latest_catalog_snapshot_with_private_records(
        &self,
        current: &crate::DurableCatalogSnapshot,
        latest: &Engine,
    ) -> StorageBackendResult<crate::DurableCatalogSnapshot> {
        use uqa_execution::catalog::security::system_relations;
        let mut snapshot = latest.durable.snapshot();
        let sequences = snapshot.sequence_read_snapshot().merge_private(
            self.storage.catalog.as_deref(),
            &current.sequence_read_snapshot(),
        )?;
        snapshot.roles = sequences.roles.roles;
        snapshot.role_memberships = sequences.roles.memberships;
        snapshot.sequences = sequences.sequences;
        snapshot.sequence_object_ids = sequences.object_ids;
        snapshot.sequence_persistence = sequences.persistence;
        snapshot.sequence_security = sequences.security;
        snapshot.schemas = uqa_execution::schema::namespaces::authority::merge_private(
            self.storage.catalog.as_deref(),
            &current.schemas,
            snapshot.schemas,
            &snapshot.roles,
        )?;
        snapshot.domains = uqa_execution::catalog::domain::merge_private(
            self.storage.catalog.as_deref(),
            &current.domains,
            snapshot.domains,
            &snapshot.roles,
        )?;
        snapshot.enums = uqa_execution::catalog::enum_type::merge_private(
            self.storage.catalog.as_deref(),
            &current.enums,
            snapshot.enums,
            &snapshot.roles,
        )?;
        snapshot.composites = uqa_execution::catalog::composite_type::merge_private(
            self.storage.catalog.as_deref(),
            &current.composites,
            snapshot.composites,
            &snapshot.roles,
        )?;
        snapshot.sql_user_functions = uqa_execution::routines::catalog::merge_private(
            self.storage.catalog.as_deref(),
            &current.sql_user_functions,
            snapshot.sql_user_functions,
            &snapshot.roles,
        )?;
        snapshot.system_relation_security = Arc::new(system_relations::merge_private(
            self.storage.catalog.as_deref(),
            &current.system_relation_security,
            (*snapshot.system_relation_security).clone(),
        )?);
        snapshot.views = uqa_execution::catalog::security::relation_authority::merge_private_views(
            self.storage.catalog.as_deref(),
            &current.views,
            snapshot.views,
        )?;
        snapshot.foreign_table_security =
            uqa_execution::catalog::security::relation_authority::merge_private_foreign(
                self.storage.catalog.as_deref(),
                &current.foreign_tables,
                &current.foreign_table_security,
                &snapshot.foreign_tables,
                snapshot.foreign_table_security,
            )?;
        Ok(snapshot)
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
        let latest_durable =
            self.latest_catalog_snapshot_with_private_records(&previous_durable, latest)?;
        self.durable.restore(&latest_durable);
        self.rebind_graph_stores()?;
        self.durable.views.write().extend(temporary_views);
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
        self.invalidate_prepared_analysis();
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

        let latest = self.new_internal_read_session()?;
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

        if self.refresh_tracked_storage_snapshot()? {
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

    /// Reload the table catalog and the registries after a rollback, pushing their failures to `cleanup_errors`. Both read one snapshot: the open transaction's, or else a read transaction pinned for them, since separate captures would leave the reloaded tables at different states when another session commits in between. When both succeed, the cache revisions and the read view they read are recorded, as a refresh records them, so the next statement refreshes only what other sessions committed since instead of reloading every table again.
    pub(crate) fn reload_catalogs_after_rollback(&self, cleanup_errors: &mut Vec<String>) {
        let Some(backend) = self.storage.backend.as_ref() else {
            return;
        };
        let table_data_epoch = self
            .epochs
            .table_data
            .published
            .load(std::sync::atomic::Ordering::Acquire);
        let in_transaction = backend.in_transaction();
        let (pinned, stable_version) = if in_transaction {
            (false, None)
        } else {
            match Self::pin_rollback_snapshot(backend.as_ref()) {
                Ok(stable_version) => (true, stable_version),
                Err(error) => {
                    cleanup_errors.push(format!("rollback read snapshot: {error}"));
                    (false, None)
                }
            }
        };
        let invalidation = if pinned || in_transaction {
            match self.rollback_prepared_invalidation() {
                Ok(invalidation) => invalidation,
                Err(error) => {
                    cleanup_errors.push(format!("rollback catalog revisions: {error}"));
                    PreparedInvalidation::Analysis
                }
            }
        } else {
            PreparedInvalidation::Analysis
        };
        let tables = match invalidation {
            PreparedInvalidation::Analysis => self.reload_table_catalog_after_rollback(),
            PreparedInvalidation::Execution => self.restore_rolled_back_table_catalog(invalidation),
        };
        if let Err(error) = &tables {
            cleanup_errors.push(format!("table catalog restore: {error}"));
        }
        let registries = self.reload_catalog_registries_after_rollback(invalidation);
        if let Err(error) = &registries {
            cleanup_errors.push(format!("registry restore: {error}"));
        }
        if tables.is_ok() && registries.is_ok() && (pinned || in_transaction) {
            if let Err(error) = self.record_reloaded_snapshot(table_data_epoch, stable_version) {
                cleanup_errors.push(format!("cache revision record: {error}"));
            }
        }
        if pinned {
            if let Err(error) = backend.rollback_transaction() {
                cleanup_errors.push(format!("rollback read snapshot release: {error}"));
            }
        }
    }

    fn rollback_prepared_invalidation(&self) -> StorageBackendResult<PreparedInvalidation> {
        let binding_revisions = |revisions: &uqa_storage::CatalogCacheRevisions| {
            (
                revisions.table_catalog,
                revisions.registries,
                revisions.graphs.clone(),
                revisions.storage_schema,
            )
        };
        let previous = self
            .epochs
            .storage_cache_revisions
            .lock()
            .as_ref()
            .map(binding_revisions);
        let current = self
            .storage
            .catalog
            .as_ref()
            .map(|catalog| catalog.cache_revisions())
            .transpose()?
            .flatten();
        // Equal binding generations prove that rollback changed data alone. The physical stores still need their full restoration; only the analyzed SQL inputs remain valid.
        Ok(
            if previous.is_some() && previous == current.as_ref().map(binding_revisions) {
                PreparedInvalidation::Execution
            } else {
                PreparedInvalidation::Analysis
            },
        )
    }

    /// Begin a read transaction and pin its snapshot. Returns the commit version when the monitor shows that no commit came between the two reads around the pin, as a refresh reads them.
    fn pin_rollback_snapshot(
        backend: &dyn uqa_storage::PersistentStorageBackend,
    ) -> StorageBackendResult<Option<u64>> {
        backend.begin_read_transaction()?;
        let pinned = (|| {
            if !backend.change_version_monitor_is_nonblocking()? {
                backend.pin_transaction_snapshot()?;
                return Ok(None);
            }
            let before = backend.change_version()?;
            backend.pin_transaction_snapshot()?;
            let after = backend.change_version()?;
            Ok(before.filter(|before| after == Some(*before)))
        })();
        if pinned.is_err() {
            let _ = backend.rollback_transaction();
        }
        pinned
    }

    /// Record what the reload after a rollback read: the catalog's cache revisions, the read view, the data epoch it loaded, and the commit version when it stood still around the pin.
    fn record_reloaded_snapshot(
        &self,
        table_data_epoch: u64,
        stable_version: Option<u64>,
    ) -> StorageBackendResult<()> {
        let revisions = match self.storage.catalog.as_ref() {
            Some(catalog) => catalog.cache_revisions()?,
            None => None,
        };
        let read_view = self
            .storage
            .backend
            .as_ref()
            .map(|backend| backend.read_view_revision())
            .transpose()?
            .flatten();
        *self.epochs.storage_cache_revisions.lock() = revisions;
        *self.epochs.seen_storage_read_view.lock() = read_view;
        self.epochs
            .table_data
            .seen
            .store(table_data_epoch, std::sync::atomic::Ordering::Release);
        if let Some(version) = stable_version {
            self.epochs
                .seen_storage_change_version
                .store(version, std::sync::atomic::Ordering::Release);
        }
        Ok(())
    }

    pub(crate) fn reload_table_catalog_after_rollback(&self) -> StorageBackendResult<()> {
        self.restore_rolled_back_table_catalog(PreparedInvalidation::Analysis)
    }

    fn restore_rolled_back_table_catalog(
        &self,
        invalidation: PreparedInvalidation,
    ) -> StorageBackendResult<()> {
        // First, so that a failed reload cannot leave a count ahead of its store.
        self.discard_persistent_document_counts();
        *self.epochs.storage_cache_revisions.lock() = None;
        self.clear_persistent_table_bindings_for_catalog_reload();
        let target_epoch = self
            .epochs
            .table_catalog
            .published
            .load(std::sync::atomic::Ordering::Acquire);
        self.reload_table_catalog_with_invalidation(target_epoch, invalidation)?;
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
        let temporary_schema = self.temporary_schema_name();
        self.durable
            .table_field_analyzers
            .write()
            .retain(|(table, _), _| {
                crate::RelationIdentity::from_legacy_name(table)
                    .is_ok_and(|relation| relation.schema == temporary_schema)
            });
        self.durable
            .catalog_indexes
            .write()
            .retain(|relation, _| relation.schema == temporary_schema);
    }

    pub(super) fn reload_table_catalog(&self, target_epoch: u64) -> StorageBackendResult<()> {
        self.reload_table_catalog_with_invalidation(target_epoch, PreparedInvalidation::Analysis)
    }

    fn reload_table_catalog_with_invalidation(
        &self,
        target_epoch: u64,
        invalidation: PreparedInvalidation,
    ) -> StorageBackendResult<()> {
        self.clear_regtype_output_cache();
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

        uqa_execution::schema::constraints::restoration::validate_constraint_catalog(
            catalog.as_ref(),
        )?;
        self.restore_roles_from_metadata(catalog.as_ref(), false)?;
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
        let names = uqa_execution::schema::indexes::constraint_names::KeyConstraintNames::load(
            catalog.as_ref(),
        )?;
        for (schema, security) in
            uqa_execution::catalog::security::relation_restoration::restore_tables(
                catalog.as_ref(),
                &self.durable.roles.read(),
                false,
            )?
        {
            let relation = schema.relation.clone();
            let table = Self::load_session_table(
                catalog.as_ref(),
                backend.as_ref(),
                schema,
                security,
                &names,
            )?;
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
        invalidation.apply(self);
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
        if self.refresh_tracked_storage_snapshot()? {
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

    fn reload_catalog_registries_after_rollback(
        &self,
        invalidation: PreparedInvalidation,
    ) -> StorageBackendResult<()> {
        let target_epoch = self
            .epochs
            .catalog_registry
            .published
            .load(std::sync::atomic::Ordering::Acquire);
        self.reload_catalog_registries_with_invalidation(target_epoch, invalidation)?;
        self.epochs
            .catalog_registry
            .dirty
            .store(false, std::sync::atomic::Ordering::Release);
        Ok(())
    }

    pub(super) fn reload_catalog_registries(&self, target_epoch: u64) -> StorageBackendResult<()> {
        self.reload_catalog_registries_with_invalidation(
            target_epoch,
            PreparedInvalidation::Analysis,
        )
    }

    fn reload_catalog_registries_with_invalidation(
        &self,
        target_epoch: u64,
        invalidation: PreparedInvalidation,
    ) -> StorageBackendResult<()> {
        self.clear_regtype_output_cache();
        self.clear_bayesian_params_cache();
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
        *self.durable.views.write() = temporary_views;
        self.clear_persistent_table_bindings_for_catalog_reload();
        self.durable.schemas.write().clear();
        self.durable.path_indexes.write().clear();
        self.durable.named_analyzers.write().clear();
        self.durable.foreign_servers.write().clear();
        self.durable.foreign_tables.write().clear();
        self.durable.foreign_table_security.write().clear();
        self.durable.system_relation_security.write().clear();
        self.durable.sql_user_functions.write().clear();
        self.durable.models.write().clear();
        self.durable.scoring_params.write().clear();

        self.restore_roles_from_metadata(catalog.as_ref(), false)?;
        self.restore_schemas_from_catalog(catalog.as_ref(), super::CatalogRestoreMode::LoadOnly)?;
        self.restore_graphs_from_catalog(catalog.as_ref())?;
        self.restore_engine_registries_from_catalog(
            catalog.as_ref(),
            super::CatalogRestoreMode::LoadOnly,
        )?;
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
        invalidation.apply(self);
        Ok(())
    }
}
