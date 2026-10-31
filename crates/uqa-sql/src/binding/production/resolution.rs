//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::catalog::resolution::{RelationLookupMode, RelationNameResolution};
use crate::SQLError;
use uqa_core::memory::{MemoryReservation, Produced, ProductionControl};

/// The namespace buffers and their constructor lease move together. Lookup mode is inline state and cannot replace those buffers.
pub struct BindingResolution {
    value: RelationNameResolution,
    _memory: Option<MemoryReservation>,
}
impl BindingResolution {
    pub fn from_produced(value: Produced<RelationNameResolution>) -> Self {
        let (value, memory) = value.into_parts();
        Self {
            value,
            _memory: memory,
        }
    }
    pub fn copy_with_control(&self, control: &ProductionControl<'_>) -> Result<Self, SQLError> {
        self.value
            .copy_with_control(control)
            .map(Self::from_produced)
    }
    pub fn set_lookup_mode(&mut self, mode: RelationLookupMode) -> RelationLookupMode {
        self.value.set_lookup_mode(mode)
    }
}
impl From<RelationNameResolution> for BindingResolution {
    fn from(value: RelationNameResolution) -> Self {
        Self {
            value,
            _memory: None,
        }
    }
}
impl Clone for BindingResolution {
    fn clone(&self) -> Self {
        self.value.clone().into()
    }
}
impl std::ops::Deref for BindingResolution {
    type Target = RelationNameResolution;
    fn deref(&self) -> &Self::Target {
        &self.value
    }
}
