//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical sequential f32 arithmetic shared by ordinary and cancellable scoring.

use crate::StorageBackendResult;

#[derive(Default)]
struct Sums {
    dot: f32,
    norm_a: f32,
    norm_b: f32,
}

impl Sums {
    fn extend(&mut self, a: &[f32], b: &[f32]) {
        for (x, y) in a.iter().zip(b) {
            self.dot += x * y;
            self.norm_a += x * x;
            self.norm_b += y * y;
        }
    }

    fn finish(self) -> f32 {
        from_norms(self.dot, self.norm_a.sqrt(), self.norm_b.sqrt())
    }
}

/// Cosine similarity between equal-length vectors, using the established sequential `f32` dot and norm reductions. Empty, mismatched or zero-norm vectors produce `0.0`; derived overflow retains the existing IEEE result.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let mut sums = Sums::default();
    sums.extend(a, b);
    sums.finish()
}

/// Euclidean norm from the canonical sequential f32 sum of squared coordinates.
pub fn vector_norm(vector: &[f32]) -> f32 {
    let mut squared_norm = 0.0_f32;
    for value in vector {
        squared_norm += value * value;
    }
    squared_norm.sqrt()
}

/// Cosine similarity when both vector norms were computed once outside the
/// candidate loop. This preserves the raw-vector score while avoiding two
/// norm reductions and two square roots for every candidate.
pub fn cosine_similarity_with_norms(a: &[f32], b: &[f32], norm_a: f32, norm_b: f32) -> f32 {
    if a.len() != b.len() || a.is_empty() || norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    let mut dot = 0.0_f32;
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
    }
    from_norms(dot, norm_a, norm_b)
}

/// Score one row with a precomputed query norm, using the same sequential reductions and zero-norm behavior as ordinary cosine.
pub fn cosine_similarity_with_query_norm(query: &[f32], vector: &[f32], query_norm: f32) -> f32 {
    if query.len() != vector.len() || query_norm == 0.0 {
        return 0.0;
    }
    let mut dot = 0.0;
    let mut squared = 0.0;
    for (x, y) in query.iter().zip(vector) {
        dot += x * y;
        squared += y * y;
    }
    from_norms(dot, query_norm, squared.sqrt())
}

fn from_norms(dot: f32, norm_a: f32, norm_b: f32) -> f32 {
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a * norm_b)
    }
}

pub(crate) fn cosine_similarity_controlled(
    a: &[f32],
    b: &[f32],
    mut check: impl FnMut() -> StorageBackendResult<()>,
) -> StorageBackendResult<f32> {
    check()?;
    if a.len() != b.len() {
        return Ok(0.0);
    }
    let mut sums = Sums::default();
    for (a, b) in a.chunks(1024).zip(b.chunks(1024)) {
        check()?;
        sums.extend(a, b);
    }
    check()?;
    Ok(sums.finish())
}

#[cfg(test)]
mod tests;
