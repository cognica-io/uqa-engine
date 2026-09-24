//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Centroid ranking preserves stable ordinal ties while retaining query and probe buffers.

use super::{check, normalized_query, VectorQueryBuffer};
use crate::{read_control::StorageReadControl, StorageBackendResult};

/// Rank validated centroid vectors for the owning index. The returned probe buffer retains the supplied allowance until the provider finishes reading its selected candidates.
pub fn nearest_centroids(
    query: &[f32],
    centroids: &[Vec<f32>],
    nprobe: usize,
    control: Option<&StorageReadControl>,
) -> StorageBackendResult<VectorQueryBuffer<usize>> {
    let (normalized, _) = normalized_query(query, control)?;
    nearest_normalized_centroids(&normalized, centroids, nprobe, control)
}

pub(crate) fn nearest_normalized_centroids(
    query: &[f32],
    centroids: &[Vec<f32>],
    nprobe: usize,
    control: Option<&StorageReadControl>,
) -> StorageBackendResult<VectorQueryBuffer<usize>> {
    check(control)?;
    let mut scores = VectorQueryBuffer::new(control);
    scores.reserve(centroids.len())?;
    for (index, centroid) in centroids.iter().enumerate() {
        check(control)?;
        let score = query
            .iter()
            .zip(centroid)
            .map(|(left, right)| left * right)
            .sum::<f32>();
        scores.push((index, score))?;
    }
    scores.sort_unstable_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| left.0.cmp(&right.0))
    });
    let count = nprobe.max(1).min(scores.len());
    let mut probes = VectorQueryBuffer::new(control);
    probes.reserve(count)?;
    for &(index, _) in scores.iter().take(count) {
        check(control)?;
        probes.push(index)?;
    }
    Ok(probes)
}
