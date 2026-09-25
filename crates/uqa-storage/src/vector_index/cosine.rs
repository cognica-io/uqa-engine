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
        if self.norm_a == 0.0 || self.norm_b == 0.0 {
            0.0
        } else {
            self.dot / (self.norm_a.sqrt() * self.norm_b.sqrt())
        }
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
