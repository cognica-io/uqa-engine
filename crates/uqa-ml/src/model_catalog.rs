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

mod handle;
pub use handle::{ModelHandle, ModelIdentity};

struct ModelSource {
    model: ModelHandle,
    payload_bytes: usize,
    _name_memory: Option<MemoryReservation>,
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

    pub fn get(&self, name: &str) -> Option<&ModelHandle> {
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
                model: model.into(),
                payload_bytes,
                _name_memory: None,
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

    /// Transfer the exact decoded name, model and reservations to an unpublished native entry. Failed admission consumes this candidate; ordinary publication and capture keep using the same immutable catalog.
    pub fn insert_image(
        mut self,
        name: Budgeted<String>,
        model: Budgeted<DeepModel>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        if !name.budget().shares_allowance(control.memory())
            || !model.budget().shares_allowance(control.memory())
            || !self.values.budget().shares_allowance(control.memory())
            || name.reserved_bytes() < name.capacity()
            || self.contains_key(name.as_str())
        {
            return Err(uqa_storage::StorageBackendError::Other(
                "invalid model image admission or identity".into(),
            ));
        }
        let payload_bytes = retained_model_bytes(&model, || control.check())?;
        if model.reserved_bytes() < payload_bytes {
            return Err(uqa_storage::StorageBackendError::Other(
                "incomplete model image admission".into(),
            ));
        }
        let model = ModelHandle::from_image(model)?;
        let payload_bytes = payload_bytes
            .checked_add(model.admitted_wrapper_bytes())
            .ok_or(MemoryError::SizeOverflow)?;
        let total = name
            .capacity()
            .checked_add(payload_bytes)
            .ok_or(MemoryError::SizeOverflow)?;
        let (name, memory) = name.into_parts();
        self.values.insert(
            name,
            ModelSource {
                model,
                payload_bytes,
                _name_memory: Some(memory),
            },
            total,
        )?;
        control.check()?;
        Ok(self)
    }

    /// Select future catalog edits without moving or re-admitting retained names and model weights.
    pub fn into_mutation_budget(self, memory: &MemoryBudget) -> Self {
        Self {
            values: self.values.into_mutation_budget(memory),
        }
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
    type Output = ModelHandle;

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
