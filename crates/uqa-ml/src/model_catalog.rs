//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Model owners cache their retained footprint at publication and capture immutable catalog roots.

use crate::{
    model_image::{retained_model_bytes, ModelImageSnapshot},
    DeepModel,
};
use parking_lot::Mutex;
use std::sync::Arc;
use uqa_core::memory::{
    Budgeted, MemoryBudget, MemoryError, MemoryReservation, OwnedMap, ProductionControl,
    RetainedMap, RetainedMapSnapshot,
};
use uqa_storage::{read_control::StorageReadControl, StorageBackendResult};

struct ModelSource {
    model: Arc<DeepModel>,
    payload_bytes: usize,
}

/// Native immutable model catalog. Clones share a root; updates copy only the affected path and never traverse unrelated models or clone their weights.
#[derive(Clone)]
pub struct ModelCatalog {
    values: RetainedMap<String, ModelSource>,
}

impl Default for ModelCatalog {
    fn default() -> Self {
        Self::new(&MemoryBudget::new(usize::MAX))
    }
}

impl ModelCatalog {
    pub fn new(memory: &MemoryBudget) -> Self {
        Self {
            values: RetainedMap::new(memory),
        }
    }

    pub fn get(&self, name: &str) -> Option<&Arc<DeepModel>> {
        self.values.get(name).map(|source| &source.model)
    }

    pub fn contains_key(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    pub fn insert(&mut self, name: String, model: Arc<DeepModel>) -> Result<(), MemoryError> {
        let payload_bytes = retained_model_bytes(&model, || Ok::<_, MemoryError>(()))?;
        let total = name
            .capacity()
            .checked_add(payload_bytes)
            .ok_or(MemoryError::SizeOverflow)?;
        self.values.insert(
            name,
            ModelSource {
                model,
                payload_bytes,
            },
            total,
        )
    }

    pub fn remove(&mut self, name: &str) -> Result<bool, MemoryError> {
        self.values.remove(name)
    }

    pub fn clear(&mut self) {
        self.values.clear();
    }

    /// Independently admit the selected root in constant work without visiting names, layers or weights.
    pub fn capture(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<CapturedModelCatalog>> {
        control.check()?;
        let (values, remaining) = self.values.capture(control.memory())?.into_parts();
        control.check()?;
        Ok(Budgeted::new(
            CapturedModelCatalog {
                values,
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

impl std::ops::Index<&str> for ModelCatalog {
    type Output = Arc<DeepModel>;

    fn index(&self, name: &str) -> &Self::Output {
        self.get(name).expect("model name exists in this catalog")
    }
}

struct CapturedState {
    images: OwnedMap<String, Budgeted<ModelImageSnapshot>>,
    remaining: MemoryReservation,
}

/// Export-only model generation. Individual image handles inherit their original allowance and cancellation, and can outlive the catalog without retaining unrelated models.
pub struct CapturedModelCatalog {
    // Drop the root before the leases covering its unread models and tree nodes.
    values: RetainedMapSnapshot<String, ModelSource>,
    state: Mutex<CapturedState>,
    control: StorageReadControl,
}

impl CapturedModelCatalog {
    /// Seek the next retained identity in logarithmic time, without allocating or visiting earlier entries.
    pub fn next_name(&self, after: Option<&str>) -> StorageBackendResult<Option<&str>> {
        self.control.check()?;
        Ok(self
            .values
            .first_from(after.map_or(std::ops::Bound::Unbounded, std::ops::Bound::Excluded))
            .map(|(name, _)| name.as_str()))
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.values.iter().map(|(name, _)| name.as_str())
    }

    /// Fund a detached model from this capture's existing reservation. Only its new handle and cache entry require additional admission; repeated selection shares that handle.
    pub fn image(&self, name: &str) -> StorageBackendResult<Option<ModelImageSnapshot>> {
        self.control.check()?;
        let Some(source) = self.values.get(name) else {
            return Ok(None);
        };
        let mut state = self.state.lock();
        self.control.check()?;
        if let Some(image) = state.images.get(name) {
            return Ok(Some((**image).clone()));
        }
        let production = ProductionControl::new(
            self.control.memory(),
            self.control.cancellation(),
            self.control.cancellation(),
        );
        let (key, key_memory) = production.copy_text(name)?.into_parts();
        let key_memory = key_memory.expect("model cache keys retain their capture allowance");
        let mut memory = self
            .control
            .memory()
            .reserve(ModelImageSnapshot::wrapper_bytes())?;
        let CapturedState { images, remaining } = &mut *state;
        images.insert_budgeted_with(
            key,
            self.control.memory(),
            || -> StorageBackendResult<_> {
                self.control.check()?;
                // All fallible admission precedes transferring this model's original payload lease.
                memory.absorb(remaining.split(source.payload_bytes));
                Ok(Budgeted::new(
                    ModelImageSnapshot::from_admitted(source.model.clone(), &self.control, memory),
                    key_memory,
                ))
            },
        )?;
        Ok(Some(
            (**images.get(name).expect("selected model was cached")).clone(),
        ))
    }
}

#[cfg(test)]
mod tests;
