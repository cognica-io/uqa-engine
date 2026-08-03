//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cosine-space normalization and deterministic level selection.

pub(crate) const MAX_HNSW_LEVEL: usize = 32;

pub(super) fn normalize(vector: &[f32]) -> Vec<f32> {
    let mut normalized = vector.to_vec();
    let magnitude = normalized
        .iter()
        .map(|value| value * value)
        .sum::<f32>()
        .sqrt();
    if magnitude > 1.0e-12 {
        for value in &mut normalized {
            *value /= magnitude;
        }
    }
    normalized
}

pub(super) fn distance(left: &[f32], right: &[f32]) -> f32 {
    -left
        .iter()
        .zip(right)
        .map(|(left, right)| left * right)
        .sum::<f32>()
}

pub(super) fn deterministic_level(seed: u64, node_id: u64, m: usize) -> usize {
    let random = splitmix64(seed ^ node_id.wrapping_mul(0x9e37_79b9_7f4a_7c15));
    let mantissa = (random >> 11).saturating_add(1);
    let uniform = mantissa as f64 / ((1_u64 << 53) as f64 + 1.0);
    let level = (-uniform.ln() / (m as f64).ln()).floor();
    if level.is_finite() && level > 0.0 {
        (level as usize).min(MAX_HNSW_LEVEL)
    } else {
        0
    }
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}
