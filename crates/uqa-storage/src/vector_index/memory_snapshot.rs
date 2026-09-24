//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Memory vector snapshots reserve copied tensors before publishing an immutable reader.

use std::sync::Arc;

use uqa_core::memory::{Budgeted, BudgetedVec};

use super::{MemoryVectorIndex, RetainedVectorIndexBuilder, VectorIndex};
use crate::{read_control::StorageReadControl, StorageBackendResult};

pub(super) fn capture(
    source: &MemoryVectorIndex,
    control: &StorageReadControl,
) -> StorageBackendResult<Arc<dyn VectorIndex>> {
    control.check()?;
    let mut builder = RetainedVectorIndexBuilder::new(source.dimensions, control);
    for (&id, vectors) in &source.vectors {
        control.check()?;
        // Tensor buffers must drop before their payload reservation on every failure path.
        let mut pending = (
            BudgetedVec::new(control.memory()),
            control.memory().empty_reservation(),
        );
        for vector in vectors {
            control.check()?;
            let mut copied = BudgetedVec::new(control.memory());
            copied.extend_from_slice(vector)?;
            let (copied, memory) = copied.into_parts();
            pending.0.push(copied)?;
            pending.1.absorb(memory);
        }
        let (vectors, mut memory) = pending.0.into_parts();
        memory.absorb(pending.1);
        builder.add_document(id, Budgeted::new(vectors, memory))?;
    }
    Ok(Arc::new(builder.finish()?))
}

#[cfg(test)]
mod tests;
