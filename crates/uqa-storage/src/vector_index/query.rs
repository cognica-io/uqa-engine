//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Vector query workspace retains the selected reader's allowance through scoring and result construction.

mod buffer;
mod centroids;
mod containers;
mod scores;

#[cfg(test)]
mod tests;

pub use buffer::VectorQueryBuffer;
pub use centroids::nearest_centroids;
pub(crate) use centroids::nearest_normalized_centroids;
pub(crate) use containers::{QueryHeap, QuerySet};
pub use scores::scored_posting_list;
pub(crate) use scores::{postings_from_scores, postings_from_unique_scores};

use crate::{read_control::StorageReadControl, StorageBackendResult};

pub(crate) fn normalized_query(
    query: &[f32],
    control: Option<&StorageReadControl>,
) -> StorageBackendResult<(VectorQueryBuffer<f32>, f32)> {
    check(control)?;
    let mut normalized = VectorQueryBuffer::new(control);
    normalized.extend_from_slice(query)?;
    let norm = super::vector_norm(query);
    if norm > 1.0e-12 {
        for (index, value) in normalized.iter_mut().enumerate() {
            if index.is_multiple_of(1024) {
                check(control)?;
            }
            *value /= norm;
        }
    }
    check(control)?;
    Ok((normalized, norm))
}

pub(crate) fn check(control: Option<&StorageReadControl>) -> StorageBackendResult<()> {
    if let Some(control) = control {
        control.check()?;
    }
    Ok(())
}
