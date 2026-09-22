//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Normalization, centroid assignment, and deterministic k-means.

use super::prepare::check;
use crate::vector_index::vector_norm;
use crate::{read_control::StorageReadControl, StorageBackendResult};

pub(super) fn l2_normalize(vector: &mut [f32]) -> f32 {
    let magnitude = vector_norm(vector);
    if magnitude > 1e-12 {
        for value in vector {
            *value /= magnitude;
        }
    }
    magnitude
}

pub(super) fn nearest_centroid_controlled(
    vector: &[f32],
    centroids: &[Vec<f32>],
    control: Option<&StorageReadControl>,
) -> StorageBackendResult<usize> {
    let mut best_index = 0;
    let mut best_similarity = f32::NEG_INFINITY;
    for (index, centroid) in centroids.iter().enumerate() {
        check(control)?;
        let mut similarity = 0.0;
        for (offset, (left, right)) in vector.iter().zip(centroid).enumerate() {
            if offset.is_multiple_of(1024) {
                check(control)?;
            }
            similarity += left * right;
        }
        if similarity > best_similarity {
            best_similarity = similarity;
            best_index = index;
        }
    }
    Ok(best_index)
}

pub(super) fn kmeans(
    vectors: &[Vec<f32>],
    cluster_count: usize,
    dimensions: usize,
    iterations: usize,
    control: Option<&StorageReadControl>,
) -> StorageBackendResult<Vec<Vec<f32>>> {
    if vectors.is_empty() || cluster_count == 0 {
        return Ok(Vec::new());
    }
    let stride = (vectors.len() / cluster_count).max(1);
    let mut centroids = (0..cluster_count)
        .map(|index| vectors[(index * stride) % vectors.len()].clone())
        .collect::<Vec<_>>();
    for _ in 0..iterations {
        check(control)?;
        let mut sums = vec![vec![0.0; dimensions]; cluster_count];
        let mut counts = vec![0_usize; cluster_count];
        for vector in vectors {
            let cluster = nearest_centroid_controlled(vector, &centroids, control)?;
            for (sum, value) in sums[cluster].iter_mut().zip(vector) {
                *sum += value;
            }
            counts[cluster] += 1;
        }
        for (cluster, centroid) in centroids.iter_mut().enumerate() {
            check(control)?;
            if counts[cluster] == 0 {
                continue;
            }
            for (value, sum) in centroid.iter_mut().zip(&sums[cluster]) {
                *value = *sum / counts[cluster] as f32;
            }
            l2_normalize(centroid);
        }
    }
    Ok(centroids)
}
