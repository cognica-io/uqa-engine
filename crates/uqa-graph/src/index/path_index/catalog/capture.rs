//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent capture admits the native root once and transfers selected payload leases to escaping children.

use super::PathIndexCatalog;
use crate::index::path_index::{PathIndex, PathIndexImageSnapshot};
use parking_lot::Mutex;
use std::sync::Arc;
use uqa_core::memory::{
    Budgeted, MemoryReservation, OwnedMap, Produced, ProductionControl, RetainedMapSnapshot,
};
use uqa_storage::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};

impl PathIndexCatalog {
    /// Capturing materialized memory indexes takes constant work and allocates no membership or payload nodes. Non-memory handles belong to their provider snapshot and are rejected without enumeration or graph evaluation.
    pub fn capture(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<CapturedPathIndexes>> {
        control.check()?;
        if self.non_memory != 0 {
            return Err(StorageBackendError::Other(
                "memory path-index capture requires materialized memory storage".into(),
            ));
        }
        let (indexes, remaining) = self.indexes.capture(control.memory())?.into_parts();
        control.check()?;
        Ok(Budgeted::new(
            CapturedPathIndexes {
                indexes,
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
    images: OwnedMap<String, Budgeted<PathIndexImageSnapshot>>,
    remaining: MemoryReservation,
}

pub struct CapturedPathIndexes {
    indexes: RetainedMapSnapshot<Produced<String>, PathIndex>,
    state: Mutex<CapturedState>,
    control: StorageReadControl,
}

impl CapturedPathIndexes {
    pub fn len(&self) -> usize {
        self.indexes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.indexes.is_empty()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.indexes.iter().map(|(name, _)| name.as_str())
    }

    pub fn image(&self, name: &str) -> StorageBackendResult<Option<PathIndexImageSnapshot>> {
        self.control.check()?;
        let Some(index) = self.indexes.get(name) else {
            return Ok(None);
        };
        let mut state = self.state.lock();
        self.control.check()?;
        if let Some(image) = state.images.get(name) {
            return Ok(Some((**image).clone()));
        }
        let paths = index.memory_paths()?;
        let production = ProductionControl::new(
            self.control.memory(),
            self.control.cancellation(),
            self.control.cancellation(),
        );
        let (key, key_memory) = production.copy_text(name)?.into_parts();
        let key_memory = key_memory.expect("path image cache retains its capture allowance");
        let mut memory = self
            .control
            .memory()
            .reserve(PathIndexImageSnapshot::wrapper_bytes())?;
        let CapturedState { images, remaining } = &mut *state;
        images.insert_budgeted_with(
            key,
            self.control.memory(),
            || -> StorageBackendResult<_> {
                self.control.check()?;
                memory.absorb(remaining.split(paths.retained_bytes()));
                Ok(Budgeted::new(
                    PathIndexImageSnapshot::from_admitted(Arc::clone(paths), &self.control, memory),
                    key_memory,
                ))
            },
        )?;
        Ok(Some(
            (**images.get(name).expect("selected path image was cached")).clone(),
        ))
    }
}
