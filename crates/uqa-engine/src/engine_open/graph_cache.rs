//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable graph versions shared across sessions, with one loader per graph.

use std::collections::BTreeMap;
use std::sync::{Arc, Weak};

use parking_lot::Mutex;
use uqa_graph::MemoryGraphStore;
use uqa_storage::StorageBackendResult;

type Versions = BTreeMap<u64, Weak<MemoryGraphStore>>;
type Slot = Arc<Mutex<Versions>>;

#[derive(Default)]
pub(crate) struct GraphSnapshots {
    slots: Mutex<BTreeMap<String, Slot>>,
    #[cfg(test)]
    loads: std::sync::atomic::AtomicUsize,
}

impl GraphSnapshots {
    pub(crate) fn load(
        &self,
        name: &str,
        revision: u64,
        load: impl FnOnce() -> StorageBackendResult<Option<MemoryGraphStore>>,
    ) -> StorageBackendResult<Option<Arc<MemoryGraphStore>>> {
        let slot = Arc::clone(self.slots.lock().entry(name.to_owned()).or_default());
        let mut versions = slot.lock();
        if let Some(graph) = versions.get(&revision).and_then(Weak::upgrade) {
            return Ok(Some(graph));
        }
        #[cfg(test)]
        self.loads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let graph = load()?.map(Arc::new);
        // Old transaction snapshots keep their own versions alive. Retain
        // those versions without making this cache an unbounded graph owner.
        versions.retain(|_, graph| graph.strong_count() != 0);
        if let Some(graph) = &graph {
            versions.insert(revision, Arc::downgrade(graph));
        }
        Ok(graph)
    }

    #[cfg(test)]
    pub(crate) fn load_count(&self) -> usize {
        self.loads.load(std::sync::atomic::Ordering::Relaxed)
    }
}
