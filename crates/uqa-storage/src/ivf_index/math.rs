//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Normalization, centroid assignment, and deterministic k-means.

pub(super) fn l2_normalize(vector: &mut [f32]) {
    let magnitude = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if magnitude > 1e-12 {
        for value in vector {
            *value /= magnitude;
        }
    }
}

pub(super) fn dot(left: &[f32], right: &[f32]) -> f32 {
    left.iter().zip(right).map(|(x, y)| x * y).sum()
}

pub(super) fn nearest_centroid(vector: &[f32], centroids: &[Vec<f32>]) -> usize {
    let mut best_index = 0;
    let mut best_similarity = f32::NEG_INFINITY;
    for (index, centroid) in centroids.iter().enumerate() {
        let similarity = dot(vector, centroid);
        if similarity > best_similarity {
            best_similarity = similarity;
            best_index = index;
        }
    }
    best_index
}

pub(super) fn kmeans(
    vectors: &[Vec<f32>],
    cluster_count: usize,
    dimensions: usize,
    iterations: usize,
) -> Vec<Vec<f32>> {
    if vectors.is_empty() || cluster_count == 0 {
        return Vec::new();
    }
    let stride = (vectors.len() / cluster_count).max(1);
    let mut centroids = (0..cluster_count)
        .map(|index| vectors[(index * stride) % vectors.len()].clone())
        .collect::<Vec<_>>();
    for _ in 0..iterations {
        let mut sums = vec![vec![0.0; dimensions]; cluster_count];
        let mut counts = vec![0_usize; cluster_count];
        for vector in vectors {
            let cluster = nearest_centroid(vector, &centroids);
            for (sum, value) in sums[cluster].iter_mut().zip(vector) {
                *sum += value;
            }
            counts[cluster] += 1;
        }
        for (cluster, centroid) in centroids.iter_mut().enumerate() {
            if counts[cluster] == 0 {
                continue;
            }
            for (value, sum) in centroid.iter_mut().zip(&sums[cluster]) {
                *value = *sum / counts[cluster] as f32;
            }
            l2_normalize(centroid);
        }
    }
    centroids
}
