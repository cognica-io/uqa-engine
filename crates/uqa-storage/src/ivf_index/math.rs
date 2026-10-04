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
    kmeans_source(
        vectors.len(),
        cluster_count,
        dimensions,
        iterations,
        control,
        &mut |visitor| {
            for (position, vector) in vectors.iter().enumerate() {
                visitor(position, vector)?;
            }
            Ok(())
        },
    )
}

type VectorVisitor<'a> = dyn FnMut(usize, &[f32]) -> StorageBackendResult<()> + 'a;
type VectorSource<'a> = dyn FnMut(&mut VectorVisitor<'_>) -> StorageBackendResult<()> + 'a;

pub(super) fn kmeans_source(
    count: usize,
    cluster_count: usize,
    dimensions: usize,
    iterations: usize,
    control: Option<&StorageReadControl>,
    source: &mut VectorSource<'_>,
) -> StorageBackendResult<Vec<Vec<f32>>> {
    if count == 0 || cluster_count == 0 {
        return Ok(Vec::new());
    }
    let stride = (count / cluster_count).max(1);
    let mut centroids = Vec::with_capacity(cluster_count);
    source(&mut |position, vector| {
        if centroids.len() < cluster_count && position == centroids.len() * stride {
            centroids.push(vector.to_vec());
        }
        Ok(())
    })?;
    for _ in 0..iterations {
        check(control)?;
        let mut sums = vec![vec![0.0; dimensions]; cluster_count];
        let mut counts = vec![0_usize; cluster_count];
        source(&mut |_, vector| {
            let cluster = nearest_centroid_controlled(vector, &centroids, control)?;
            for (sum, value) in sums[cluster].iter_mut().zip(vector) {
                *sum += value;
            }
            counts[cluster] += 1;
            Ok(())
        })?;
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
