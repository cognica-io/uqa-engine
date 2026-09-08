//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Restore graph definitions and bind physical sessions, never graph entities.

use super::{BTreeMap, CatalogFacade, Engine, StorageBackendError, StorageBackendResult};
use std::collections::BTreeSet;
use std::sync::Arc;

impl Engine {
    pub(crate) fn new_graph_store(&self) -> StorageBackendResult<uqa_graph::GraphStoreHandle> {
        match (&self.storage.catalog, &self.storage.backend) {
            (Some(catalog), Some(backend)) => Ok(uqa_graph::GraphStoreHandle::from_catalog(
                Arc::clone(catalog),
                Arc::clone(backend),
            )),
            (None, None) => Ok(uqa_graph::GraphStoreHandle::default()),
            _ => Err(StorageBackendError::Other(
                "graph catalog and backend must belong to one storage session".into(),
            )),
        }
    }

    /// Catalog snapshots share immutable definitions, but a durable graph's
    /// physical handle must always use the receiving session's transaction.
    pub(crate) fn rebind_graph_stores(&self) -> StorageBackendResult<()> {
        if self.storage.backend.is_none() {
            return Ok(());
        }
        let names = self
            .durable
            .graphs
            .read()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let stores = names
            .into_iter()
            .map(|name| Ok((name, Arc::new(self.new_graph_store()?))))
            .collect::<StorageBackendResult<BTreeMap<_, _>>>()?;
        self.durable.graphs.restore(&Arc::new(stores));
        let indexes = self
            .durable
            .path_indexes
            .read()
            .iter()
            .map(|(key, index)| {
                let rebound = index
                    .rebind_persistent(
                        Arc::clone(self.storage.catalog.as_ref().expect("persistent catalog")),
                        Arc::clone(self.storage.backend.as_ref().expect("persistent backend")),
                    )
                    .map_err(|error| StorageBackendError::Other(error.to_string()))?;
                Ok((key.clone(), rebound))
            })
            .collect::<StorageBackendResult<BTreeMap<_, _>>>()?;
        self.durable.path_indexes.restore(&Arc::new(indexes));
        Ok(())
    }

    pub(super) fn bind_path_index_definition(
        &self,
        key: &str,
        graph: &str,
        sequences: &[Vec<String>],
    ) -> StorageBackendResult<uqa_graph::PathIndex> {
        let catalog = self.storage.catalog.as_ref().ok_or_else(|| {
            StorageBackendError::Other("persistent path index requires a catalog".into())
        })?;
        let backend = self.storage.backend.as_ref().ok_or_else(|| {
            StorageBackendError::Other("persistent path index requires a backend".into())
        })?;
        uqa_graph::PathIndex::open_persistent(
            Arc::clone(catalog),
            Arc::clone(backend),
            key,
            graph,
            sequences,
        )
        .map_err(|error| StorageBackendError::Other(error.to_string()))
    }

    pub(super) fn restore_graphs_from_catalog(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        let names = catalog
            .load_named_graphs()?
            .into_iter()
            .collect::<BTreeSet<_>>();
        let existing = self.durable.graphs.read();
        let mut stores = BTreeMap::new();
        for name in names {
            let store = match existing.get(&name) {
                Some(store) => Arc::clone(store),
                None => Arc::new(self.new_graph_store()?),
            };
            // Labels are small catalog metadata. Entity payloads are validated
            // when a query reads the corresponding durable records.
            store
                .label_registry(&name)
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
            stores.insert(name, store);
        }
        let changed = existing.len() != stores.len() || existing.keys().ne(stores.keys());
        drop(existing);
        if changed {
            self.durable.graphs.restore(&Arc::new(stores));
        }
        Ok(())
    }

    /// Legacy ids may predate the persisted AGE label high-water marks. This
    /// explicit opening migration streams bounded pages once; load-only
    /// sessions and read transactions never perform migration writes.
    pub(super) fn migrate_graph_access_metadata(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        if catalog
            .get_metadata("graph_direct_storage_version")?
            .as_deref()
            == Some("1")
        {
            return Ok(());
        }
        let mut store = self.new_graph_store()?;
        for name in catalog.load_named_graphs()? {
            store
                .rebuild_label_registry_from_ids(&name)
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        }
        catalog.set_metadata("graph_direct_storage_version", "1")
    }

    pub(super) fn refresh_graph_handles(
        &self,
        catalog: &dyn CatalogFacade,
        previous: Option<&BTreeMap<String, u64>>,
        current: &BTreeMap<String, u64>,
    ) -> StorageBackendResult<BTreeSet<String>> {
        let changed = match previous {
            Some(previous) => previous
                .keys()
                .chain(current.keys())
                .filter(|name| previous.get(*name) != current.get(*name))
                .cloned()
                .collect(),
            None => current
                .keys()
                .chain(self.durable.graphs.read().keys())
                .cloned()
                .collect::<BTreeSet<_>>(),
        };
        if !changed.is_empty() {
            self.restore_graphs_from_catalog(catalog)?;
        }
        Ok(changed)
    }
}
