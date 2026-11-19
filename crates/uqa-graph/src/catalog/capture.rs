//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Captured graph membership admits its complete root once and transfers image leases to escaping children.

use super::GraphCatalog;
use crate::{GraphImageSnapshot, GraphStoreHandle};
use parking_lot::Mutex;
use std::sync::Arc;
use uqa_core::memory::{
    Budgeted, MemoryReservation, OwnedMap, Produced, ProductionControl, RetainedMapSnapshot,
};
use uqa_storage::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};

impl GraphCatalog {
    pub fn capture(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<CapturedGraphCatalog>> {
        control.check()?;
        if self.non_memory != 0 {
            return Err(StorageBackendError::Other(
                "persistent graph catalogs belong to their storage provider".into(),
            ));
        }
        let (graphs, remaining) = self.graphs.capture(control.memory())?.into_parts();
        control.check()?;
        Ok(Budgeted::new(
            CapturedGraphCatalog {
                graphs,
                state: Mutex::new(CapturedState {
                    images: OwnedMap::new(),
                    remaining,
                }),
                control: control.clone(),
            },
            control.memory().empty_reservation(),
        ))
    }
}

struct CapturedState {
    images: OwnedMap<String, Budgeted<GraphImageSnapshot>>,
    remaining: MemoryReservation,
}

pub struct CapturedGraphCatalog {
    graphs: RetainedMapSnapshot<Produced<String>, Arc<GraphStoreHandle>>,
    state: Mutex<CapturedState>,
    control: StorageReadControl,
}

impl CapturedGraphCatalog {
    /// Seek the next retained identity in logarithmic time, without allocating or visiting earlier entries.
    pub fn next_name(&self, after: Option<&str>) -> StorageBackendResult<Option<&str>> {
        self.control.check()?;
        Ok(self
            .graphs
            .first_from(after.map_or(std::ops::Bound::Unbounded, std::ops::Bound::Excluded))
            .map(|(name, _)| name.as_str()))
    }

    pub fn len(&self) -> usize {
        self.graphs.len()
    }
    pub fn is_empty(&self) -> bool {
        self.graphs.is_empty()
    }
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.graphs.iter().map(|(name, _)| name.as_str())
    }

    pub fn image(&self, name: &str) -> StorageBackendResult<Option<GraphImageSnapshot>> {
        self.control.check()?;
        let Some(graph) = self.graphs.get(name) else {
            return Ok(None);
        };
        let mut state = self.state.lock();
        self.control.check()?;
        if let Some(image) = state.images.get(name) {
            return Ok(Some((**image).clone()));
        }
        let bytes = graph.image_retained_bytes()?;
        let production = ProductionControl::new(
            self.control.memory(),
            self.control.cancellation(),
            self.control.cancellation(),
        );
        let (key, key_memory) = production.copy_text(name)?.into_parts();
        let key_memory = key_memory.expect("graph image cache retains its capture allowance");
        let mut memory = self
            .control
            .memory()
            .reserve(GraphImageSnapshot::wrapper_bytes())?;
        let CapturedState { images, remaining } = &mut *state;
        images.insert_budgeted_with(
            key,
            self.control.memory(),
            || -> StorageBackendResult<_> {
                self.control.check()?;
                memory.absorb(remaining.split(bytes));
                Ok(Budgeted::new(
                    GraphImageSnapshot::from_admitted(Arc::clone(graph), &self.control, memory),
                    key_memory,
                ))
            },
        )?;
        Ok(Some(
            (**images.get(name).expect("selected graph image was cached")).clone(),
        ))
    }
}
