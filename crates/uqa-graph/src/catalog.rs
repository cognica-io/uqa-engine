//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable graph-resource membership and cached native image footprints.

use crate::GraphStoreHandle;
use std::sync::Arc;
use uqa_core::memory::{MemoryBudget, MemoryError, Produced, ProductionControl, RetainedMap};
use uqa_storage::{CatalogFacade, PersistentStorageBackend, StorageBackendResult};

mod capture;
pub use capture::CapturedGraphCatalog;

#[derive(Clone)]
pub struct GraphCatalog {
    graphs: RetainedMap<Produced<String>, Arc<GraphStoreHandle>>,
    non_memory: usize,
}

impl Default for GraphCatalog {
    fn default() -> Self {
        Self::new(&MemoryBudget::new(usize::MAX))
    }
}

impl GraphCatalog {
    pub fn new(memory: &MemoryBudget) -> Self {
        Self {
            graphs: RetainedMap::new(memory),
            non_memory: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.graphs.len()
    }
    pub fn is_empty(&self) -> bool {
        self.graphs.is_empty()
    }
    pub fn get(&self, name: &str) -> Option<&Arc<GraphStoreHandle>> {
        self.graphs.get(name)
    }
    pub fn contains_key(&self, name: &str) -> bool {
        self.get(name).is_some()
    }
    pub fn iter(&self) -> impl Iterator<Item = (&String, &Arc<GraphStoreHandle>)> {
        self.graphs.iter().map(|(name, graph)| (&**name, graph))
    }
    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.iter().map(|(name, _)| name)
    }

    pub fn values(&self) -> impl Iterator<Item = &Arc<GraphStoreHandle>> {
        self.iter().map(|(_, graph)| graph)
    }

    /// Admit the selected name and native path before publishing matching membership, image footprint and provider capability.
    pub fn insert(&mut self, name: &str, graph: Arc<GraphStoreHandle>) -> StorageBackendResult<()> {
        let cancellation = uqa_core::CancellationToken::new();
        let production = ProductionControl::new(self.graphs.budget(), &cancellation, &cancellation);
        let key = production.copy_text(name)?;
        let payload = key
            .capacity()
            .checked_add(if non_memory(&graph) {
                0
            } else {
                graph.image_retained_bytes()?
            })
            .ok_or(MemoryError::SizeOverflow)?;
        let non_memory = self.non_memory - usize::from(self.get(name).is_some_and(non_memory))
            + usize::from(non_memory(&graph));
        let mut graphs = self.graphs.clone();
        graphs.insert(key, graph, payload)?;
        self.publish(Self { graphs, non_memory });
        Ok(())
    }

    pub fn remove(&mut self, name: &str) -> StorageBackendResult<bool> {
        let previous = self.get(name).is_some_and(non_memory);
        let mut graphs = self.graphs.clone();
        let removed = graphs.remove(name)?;
        self.publish(Self {
            graphs,
            non_memory: self.non_memory - usize::from(previous),
        });
        Ok(removed)
    }

    /// A replacement name and removal become one generation, including when either admission fails.
    pub fn rename(
        &mut self,
        from: &str,
        to: &str,
        graph: Arc<GraphStoreHandle>,
    ) -> StorageBackendResult<()> {
        let mut candidate = self.clone();
        candidate.remove(from)?;
        candidate.insert(to, graph)?;
        self.publish(candidate);
        Ok(())
    }

    pub fn clear(&mut self) {
        self.publish(Self::new(self.graphs.budget()));
    }

    /// Bind one selected physical view to the supplied names without observing or hydrating entities.
    pub fn bind_names<'a>(
        &self,
        names: impl IntoIterator<Item = &'a str>,
        graph: &Arc<GraphStoreHandle>,
    ) -> StorageBackendResult<Self> {
        let mut candidate = Self::new(self.graphs.budget());
        for name in names {
            candidate.insert(name, Arc::clone(graph))?;
        }
        Ok(candidate)
    }

    pub fn rebind_persistent(
        &self,
        catalog: Arc<dyn CatalogFacade>,
        backend: Arc<dyn PersistentStorageBackend>,
    ) -> StorageBackendResult<Self> {
        self.bind_names(
            self.keys().map(String::as_str),
            &Arc::new(GraphStoreHandle::from_catalog(catalog, backend)),
        )
    }

    /// Validate persisted label metadata and reconcile only changed membership. An unchanged catalog retains its existing root and needs no native node admission; a failed successor remains private.
    pub fn restored(
        &self,
        catalog: &dyn CatalogFacade,
        mut create: impl FnMut() -> StorageBackendResult<GraphStoreHandle>,
    ) -> StorageBackendResult<Option<Self>> {
        let names = catalog
            .load_named_graphs()?
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        let mut candidate = self.clone();
        let mut changed = false;
        for name in self.keys().filter(|name| !names.contains(*name)) {
            changed |= candidate.remove(name)?;
        }
        for name in names {
            let graph = if let Some(graph) = self.get(&name) {
                Arc::clone(graph)
            } else {
                let graph = Arc::new(create()?);
                candidate.insert(&name, Arc::clone(&graph))?;
                changed = true;
                graph
            };
            graph.label_registry(&name).map_err(|error| {
                uqa_storage::StorageBackendError::backend("graph catalog", error)
            })?;
        }
        Ok(changed.then_some(candidate))
    }

    fn publish(&mut self, successor: Self) {
        drop(std::mem::replace(self, successor));
    }
}

fn non_memory(graph: &Arc<GraphStoreHandle>) -> bool {
    !matches!(graph.as_ref(), GraphStoreHandle::Memory(_))
}

impl std::ops::Index<&str> for GraphCatalog {
    type Output = Arc<GraphStoreHandle>;
    fn index(&self, name: &str) -> &Self::Output {
        self.get(name).expect("graph catalog key")
    }
}

#[cfg(test)]
mod tests;
