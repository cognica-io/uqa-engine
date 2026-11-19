//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native path registration publishes an immutable membership root and cached materialization footprint.

use super::{PathIndex, PathStorage};
use crate::GraphStoreResult;
use std::sync::Arc;
use uqa_core::memory::{MemoryBudget, MemoryError, Produced, ProductionControl, RetainedMap};
use uqa_storage::{CatalogFacade, PersistentStorageBackend, StorageBackendResult};

mod capture;
mod input;
pub use capture::CapturedPathIndexes;

#[derive(Clone)]
pub struct PathIndexCatalog {
    indexes: RetainedMap<Produced<String>, PathIndex>,
    non_memory: usize,
}

impl Default for PathIndexCatalog {
    fn default() -> Self {
        Self::new(&MemoryBudget::new(usize::MAX))
    }
}

impl PathIndexCatalog {
    pub fn new(memory: &MemoryBudget) -> Self {
        Self {
            indexes: RetainedMap::new(memory),
            non_memory: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.indexes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.indexes.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&PathIndex> {
        self.indexes.get(name)
    }

    pub fn contains_key(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &PathIndex)> {
        self.indexes.iter().map(|(name, index)| (&**name, index))
    }

    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.iter().map(|(name, _)| name)
    }

    /// Admit the copied name and changed tree path before replacing the root or capability count.
    pub fn insert(&mut self, name: &str, index: PathIndex) -> StorageBackendResult<()> {
        let cancellation = uqa_core::CancellationToken::new();
        let production =
            ProductionControl::new(self.indexes.budget(), &cancellation, &cancellation);
        let key = production.copy_text(name)?;
        let payload_bytes = key
            .capacity()
            .checked_add(match &index.storage {
                PathStorage::Memory(paths) => paths.retained_bytes(),
                PathStorage::Persistent(_) | PathStorage::ReadView { .. } => 0,
            })
            .ok_or(MemoryError::SizeOverflow)?;
        let previous = self.get(name).is_some_and(non_memory);
        let non_memory = self.non_memory - usize::from(previous) + usize::from(non_memory(&index));
        let mut indexes = self.indexes.clone();
        indexes.insert(key, index, payload_bytes)?;
        self.publish(Self {
            indexes,
            non_memory,
        });
        Ok(())
    }

    pub fn remove(&mut self, name: &str) -> StorageBackendResult<bool> {
        let previous = self.get(name).is_some_and(non_memory);
        let mut indexes = self.indexes.clone();
        let removed = indexes.remove(name)?;
        self.publish(Self {
            indexes,
            non_memory: self.non_memory - usize::from(previous),
        });
        Ok(removed)
    }

    /// Build the complete successor before publishing removals. Prefix seeking avoids visiting other graph names or retaining a copied key list.
    pub fn remove_prefix(&mut self, prefix: &str) -> StorageBackendResult<()> {
        let mut candidate = self.clone();
        while let Some((name, _)) = candidate
            .indexes
            .first_from(std::ops::Bound::Included(prefix))
        {
            if !name.starts_with(prefix) {
                break;
            }
            let mut next = candidate.clone();
            next.remove(name)?;
            candidate = next;
        }
        self.publish(candidate);
        Ok(())
    }

    /// Refresh selected definitions atomically while retaining all unselected handles.
    pub fn remove_where(&mut self, selected: impl Fn(&str) -> bool) -> StorageBackendResult<()> {
        let mut candidate = self.clone();
        for name in self.keys().filter(|name| selected(name)) {
            candidate.remove(name)?;
        }
        self.publish(candidate);
        Ok(())
    }

    pub fn clear(&mut self) {
        self.publish(Self::new(self.indexes.budget()));
    }

    fn publish(&mut self, successor: Self) {
        // Install matching membership, footprint and capability before retiring any old provider handle.
        drop(std::mem::replace(self, successor));
    }

    /// Admit the complete registry successor before the physical checkpoint starts. A failed build drops the private candidate while the checkpoint restores the original definition and pairs.
    pub fn with_persistent_build(
        &self,
        catalog: Arc<dyn CatalogFacade>,
        backend: Arc<dyn PersistentStorageBackend>,
        key: &str,
        graph: &str,
        sequences: &[Vec<String>],
    ) -> GraphStoreResult<Self> {
        let index = PathIndex::open_persistent(catalog, backend, key, graph, sequences)?;
        let mut candidate = self.clone();
        candidate.insert(key, index.clone())?;
        let PathStorage::Persistent(durable) = &index.storage else {
            unreachable!("persistent constructor publishes a durable definition")
        };
        durable.materialize()?;
        Ok(candidate)
    }

    /// Rebind definitions to the receiving physical session without loading reachability rows. Partial failures cannot replace the original registry.
    pub fn rebind_persistent(
        &self,
        catalog: &Arc<dyn CatalogFacade>,
        backend: &Arc<dyn PersistentStorageBackend>,
    ) -> GraphStoreResult<Self> {
        let mut candidate = self.clone();
        for (key, index) in self.iter() {
            candidate.insert(
                key,
                index.rebind_persistent(Arc::clone(catalog), Arc::clone(backend))?,
            )?;
        }
        Ok(candidate)
    }
}

fn non_memory(index: &PathIndex) -> bool {
    !matches!(index.storage, PathStorage::Memory(_))
}

#[cfg(test)]
mod tests;
