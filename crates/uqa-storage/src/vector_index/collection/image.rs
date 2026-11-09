//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native vector registration adopts original decoded names and admits every map node.

use super::{native::NativeVectorSources, Contents, NativeIndexes, VectorIndex, VectorIndexes};
use crate::{
    memory_capture::MemoryCaptureRegistry, read_control::StorageReadControl, StorageBackendError,
    StorageBackendResult,
};
use uqa_core::memory::{Budgeted, MemoryBudget};

/// An unpublished writable registration set. Each vector already owns its decoded allocations; names and native membership use the supplied allowance. A failed addition consumes the whole candidate. Completion keeps every original reservation while subsequent mutations use the ordinary native allowance independently of import limits and cancellation.
pub struct MemoryVectorIndexesBuilder {
    indexes: NativeIndexes,
    capture: NativeVectorSources,
    control: StorageReadControl,
}

impl MemoryVectorIndexesBuilder {
    pub fn new(registry: &MemoryCaptureRegistry, control: &StorageReadControl) -> Self {
        Self {
            indexes: NativeIndexes::new(),
            capture: NativeVectorSources::new(registry, control.memory()),
            control: control.clone(),
        }
    }

    /// Adopt one unique original field and its writable evaluated index. Registering a source does not build or retrain the physical index.
    pub fn add_index(
        mut self,
        field: Budgeted<String>,
        index: Box<dyn VectorIndex>,
    ) -> StorageBackendResult<Self> {
        self.control.check()?;
        if !field.budget().shares_allowance(self.control.memory())
            || field.reserved_bytes() < field.capacity()
        {
            return Err(invalid("vector field has no complete input admission"));
        }
        if self.indexes.contains_key(field.as_str()) {
            return Err(invalid("vector image repeats a field registration"));
        }
        let index =
            index.into_memory_capture_with_control(&self.capture.registry, &self.control)?;
        self.control.check()?;
        let capture = self.capture.with_index(&field, index.as_ref())?;
        self.indexes.insert_admitted_key(field, index)?;
        self.capture = capture;
        self.control.check()?;
        Ok(self)
    }

    pub fn finish(self) -> StorageBackendResult<VectorIndexes> {
        self.control.check()?;
        Ok(VectorIndexes(Contents::Live {
            indexes: self.indexes,
            capture: Some(
                self.capture
                    .into_mutation_budget(&MemoryBudget::new(usize::MAX)),
            ),
        }))
    }
}

fn invalid(message: &'static str) -> StorageBackendError {
    StorageBackendError::Other(message.into())
}

#[cfg(test)]
mod tests;
