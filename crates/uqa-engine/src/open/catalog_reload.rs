//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One publication boundary for the table membership and logical roots of a catalog reload.

use super::{Engine, StorageBackendResult};
use crate::state::{DurableCatalogSnapshot, TableRegistrySnapshot};
use std::sync::atomic::Ordering;

struct CatalogReload<'a> {
    engine: &'a Engine,
    tables: TableRegistrySnapshot,
    durable: DurableCatalogSnapshot,
    seen: [u64; 3],
    committed: bool,
}

impl<'a> CatalogReload<'a> {
    fn begin(engine: &'a Engine) -> Self {
        Self {
            engine,
            tables: engine.storage.tables.snapshot(),
            durable: engine.durable.snapshot(),
            seen: [
                engine.epochs.table_catalog.seen.load(Ordering::Acquire),
                engine.epochs.table_data.seen.load(Ordering::Acquire),
                engine.epochs.catalog_registry.seen.load(Ordering::Acquire),
            ],
            committed: false,
        }
    }

    fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for CatalogReload<'_> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let engine = self.engine;
        engine.storage.tables.restore(&self.tables);
        engine.durable.restore(&self.durable);
        for (channel, seen) in [
            &engine.epochs.table_catalog,
            &engine.epochs.table_data,
            &engine.epochs.catalog_registry,
        ]
        .into_iter()
        .zip(self.seen)
        {
            channel.seen.store(seen, Ordering::Release);
        }
        // Partly rebound physical stores remain unqualified until the next complete refresh.
        *engine.epochs.storage_cache_revisions.lock() = None;
        *engine.epochs.seen_storage_read_view.lock() = None;
        engine.clear_regtype_output_cache();
        engine.clear_bayesian_params_cache();
        engine.clear_sql_statement_cache();
    }
}

impl Engine {
    /// The invoking statement or transaction owns admission; all selected catalog reloads publish together or retain their original logical roots and membership.
    pub(super) fn reload_catalog_snapshot(
        &self,
        table_epoch: Option<u64>,
        registry_epoch: Option<u64>,
    ) -> StorageBackendResult<()> {
        let reload = CatalogReload::begin(self);
        if registry_epoch.is_some() && self.storage.catalog.is_some() {
            self.clear_persistent_table_bindings_for_catalog_reload();
        }
        if let Some(epoch) = table_epoch {
            self.reload_table_catalog(epoch)?;
            self.synchronize_partition_identity_watermarks()?;
        }
        if let Some(epoch) = registry_epoch {
            self.reload_catalog_registries(epoch)?;
        }
        reload.commit();
        Ok(())
    }
}
