//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table-definition and durable-registry epoch synchronization.

use super::{BTreeMap, DeepModel, Engine, StorageBackendError, StorageBackendResult};

impl Engine {
    /// Mark a durable non-table registry change. Explicit transactions keep
    /// the generation private until their outer COMMIT; autocommit operations
    /// publish immediately.
    pub(crate) fn note_catalog_registry_changed(&self) {
        self.clear_regtype_output_cache();
        self.clear_bayesian_params_cache();
        if !self.session.transactions.lock().is_empty() {
            self.epochs
                .catalog_registry
                .dirty
                .store(true, std::sync::atomic::Ordering::Release);
            return;
        }
        self.publish_catalog_registry_changes();
    }

    pub(crate) fn publish_catalog_registry_changes(&self) {
        self.clear_bayesian_params_cache();
        self.epochs
            .catalog_registry
            .published
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        self.epochs
            .catalog_registry
            .dirty
            .store(false, std::sync::atomic::Ordering::Release);
        self.clear_sql_statement_cache();
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

        let mut rebound = BTreeMap::new();
        for schema in catalog.load_tables()? {
            let relation = schema.relation.clone();
            rebound.insert(
                relation,
                Self::load_session_table(catalog.as_ref(), backend.as_ref(), schema)?,
            );
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
