//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Controlled readers share an immutable root while admitting its complete node and tensor capacities.

use super::{MemoryVectorIndex, VectorIndex};
use crate::{read_control::StorageReadControl, ReadOnlySnapshot, StorageBackendResult};
use std::sync::Arc;
use uqa_core::memory::{Budgeted, MemoryError};

mod canonical;

pub(super) fn capture(
    source: &MemoryVectorIndex,
    control: &StorageReadControl,
) -> StorageBackendResult<Arc<dyn VectorIndex>> {
    control.check()?;
    let bytes =
        usize::try_from(source.state.retained_bytes()).map_err(|_| MemoryError::SizeOverflow)?;
    let memory = control.memory().reserve(bytes)?;
    let fingerprint = source.fingerprint(control)?;
    let snapshot =
        ReadOnlySnapshot::from_budgeted(Budgeted::new(source.shared_snapshot(), memory))?
            .with_vector_read_control(control)?
            .with_canonical_fingerprint(fingerprint, Some(control))?;
    control.check()?;
    Ok(Arc::new(snapshot))
}

#[cfg(test)]
mod tests;
