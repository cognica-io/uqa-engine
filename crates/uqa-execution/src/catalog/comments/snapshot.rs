//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Original comment eligibility is part of the native description generation.

use super::{CapturedComments, CommentRegistry};
use uqa_core::memory::ProductionControl;
use uqa_storage::{
    memory_capture::{CapturedMemoryValue, MemoryCaptureSource},
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};

pub struct CommentCaptureSource {
    comments: CommentRegistry,
}

impl CommentCaptureSource {
    /// Retain the original durable root without enumerating descriptions or their addressed objects.
    pub fn new(comments: &CommentRegistry) -> Self {
        Self {
            comments: comments.durable_generation(),
        }
    }
}

impl MemoryCaptureSource for CommentCaptureSource {
    type Frozen = CapturedMemoryValue<CapturedComments>;

    fn freeze(&self, control: &StorageReadControl) -> StorageBackendResult<Self::Frozen> {
        let production = ProductionControl::new(
            control.memory(),
            control.cancellation(),
            control.cancellation(),
        );
        let value = self
            .comments
            .capture(&production)
            .map_err(|error| StorageBackendError::backend("comment catalog capture", error))?
            .into_budgeted()
            .unwrap_or_else(|_| unreachable!("comment capture owns its allowance"));
        CapturedMemoryValue::new(value, control)
    }
}

#[cfg(test)]
mod tests;
