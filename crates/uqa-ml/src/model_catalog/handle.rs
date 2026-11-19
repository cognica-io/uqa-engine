//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared immutable models keep decoded weights and their input reservation inseparable.

use crate::DeepModel;
use std::{
    ops::Deref,
    sync::{Arc, Weak},
};
use uqa_core::memory::{Budgeted, MemoryError};

#[derive(Clone)]
enum Owner {
    Native(Arc<DeepModel>),
    Image(Arc<Budgeted<DeepModel>>),
}

/// A shared model value. Cloning this handle never copies weights or releases their original admission; cloning its dereferenced model creates independent native weights.
#[derive(Clone)]
pub struct ModelHandle(Owner);

impl From<Arc<DeepModel>> for ModelHandle {
    fn from(model: Arc<DeepModel>) -> Self {
        Self(Owner::Native(model))
    }
}

impl Deref for ModelHandle {
    type Target = DeepModel;
    fn deref(&self) -> &DeepModel {
        match &self.0 {
            Owner::Native(model) => model,
            Owner::Image(model) => model,
        }
    }
}

impl ModelHandle {
    pub(super) fn from_image(model: Budgeted<DeepModel>) -> Result<Self, MemoryError> {
        Ok(Self(Owner::Image(model.into_shared()?)))
    }

    pub fn ptr_eq(left: &Self, right: &Self) -> bool {
        match (&left.0, &right.0) {
            (Owner::Native(left), Owner::Native(right)) => Arc::ptr_eq(left, right),
            (Owner::Image(left), Owner::Image(right)) => Arc::ptr_eq(left, right),
            _ => false,
        }
    }

    pub fn identity(&self) -> ModelIdentity {
        ModelIdentity(match &self.0 {
            Owner::Native(model) => Identity::Native(Arc::downgrade(model)),
            Owner::Image(model) => Identity::Image(Arc::downgrade(model)),
        })
    }

    pub fn capture_image_with_control(
        &self,
        control: &uqa_storage::read_control::StorageReadControl,
    ) -> uqa_storage::StorageBackendResult<crate::model_image::ModelImageSnapshot> {
        control.check()?;
        let bytes = crate::model_image::retained_model_bytes(self, || control.check())?
            .checked_add(self.admitted_wrapper_bytes())
            .ok_or(MemoryError::SizeOverflow)?;
        let mut memory = control.memory().reserve(bytes)?;
        memory.grow(crate::model_image::ModelImageSnapshot::wrapper_bytes())?;
        control.check()?;
        Ok(crate::model_image::ModelImageSnapshot::from_admitted(
            self.clone(),
            control,
            memory,
        ))
    }

    pub(super) fn admitted_wrapper_bytes(&self) -> usize {
        match self.0 {
            Owner::Native(_) => 0,
            Owner::Image(_) => size_of::<Budgeted<DeepModel>>() - size_of::<DeepModel>(),
        }
    }
}

enum Identity {
    Native(Weak<DeepModel>),
    Image(Weak<Budgeted<DeepModel>>),
}

/// Non-owning lifetime observation; this identity cannot read the model or keep its weights alive.
pub struct ModelIdentity(Identity);

impl ModelIdentity {
    pub fn is_live(&self) -> bool {
        match &self.0 {
            Identity::Native(model) => model.strong_count() != 0,
            Identity::Image(model) => model.strong_count() != 0,
        }
    }
}
