//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Per-insertion scratch depends on vector width and construction breadth, not graph size.

use super::{HNSWIndex, HNSWMutation, StorageReadControl};
use crate::hnsw_index::{search::Candidate, types::NodeId};
use crate::{vector_index::validate_vector_values, StorageBackendResult};
use uqa_core::memory::{MemoryError, MemoryReservation};

pub(super) fn validate_mutations(
    dimensions: u32,
    mutations: &[HNSWMutation<'_>],
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    for mutation in mutations {
        control.check()?;
        if let HNSWMutation::Replace { vectors, .. } = mutation {
            for vector in *vectors {
                control.check()?;
                validate_vector_values(dimensions, vector)?;
            }
        }
    }
    Ok(())
}

pub(in crate::hnsw_index) fn operation(
    index: &HNSWIndex,
    control: &StorageReadControl,
) -> StorageBackendResult<MemoryReservation> {
    control.check()?;
    // Raw input, normalization and reciprocal-neighbor copies coexist. Heaps, visited membership, graph records and decoded pages retain their own reservations.
    let vectors = product(index.dimensions as usize, 3 * size_of::<f32>())?;
    let candidates = product(
        index.params.m.saturating_mul(2).saturating_add(1),
        2 * size_of::<Candidate>() + size_of::<[NodeId; 8]>(),
    )?;
    let degrees = product(index.params.m, 16 * size_of::<NodeId>())?;
    Ok(control.memory().reserve(
        vectors
            .checked_add(candidates)
            .and_then(|n| n.checked_add(degrees))
            .ok_or(MemoryError::SizeOverflow)?,
    )?)
}

pub(super) fn product(left: usize, right: usize) -> Result<usize, MemoryError> {
    left.checked_mul(right).ok_or(MemoryError::SizeOverflow)
}
