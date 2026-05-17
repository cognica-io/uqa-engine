//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Vector similarity to probability conversion. Rust implementation of
//! `uqa.scoring.vector`.
//!
//! Two modes:
//!
//! * Uncalibrated (Definition 7.1.2, Paper 3):
//!   `P_vector = (1 + cos_sim) / 2`.
//! * Calibrated (Theorem 3.1.1, Paper 5): likelihood-ratio calibration
//!   via [`crate::calibration::VectorProbabilityTransform`] -- the
//!   caller supplies a pre-fitted transform built from a background
//!   distance distribution.
//!
//! The Rust API is associated functions on [`VectorScorer`]; there is
//! no instance state. This matches the canonical UQA behavior's static-only
//! shape.

use crate::calibration::VectorProbabilityTransform;

pub struct VectorScorer;

impl VectorScorer {
    /// Cosine similarity between two equal-length vectors. Returns 0
    /// when either side has zero norm.
    pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
        if a.len() != b.len() {
            return 0.0;
        }
        let mut dot = 0.0_f64;
        let mut na = 0.0_f64;
        let mut nb = 0.0_f64;
        for (x, y) in a.iter().zip(b.iter()) {
            let x = f64::from(*x);
            let y = f64::from(*y);
            dot += x * y;
            na += x * x;
            nb += y * y;
        }
        let denom = na.sqrt() * nb.sqrt();
        if denom == 0.0 {
            0.0
        } else {
            dot / denom
        }
    }

    /// `P_vector = (1 + score) / 2` -- Definition 7.1.2.
    pub fn similarity_to_probability(cosine_sim: f64) -> f64 {
        f64::midpoint(1.0 + cosine_sim, 0.0)
    }

    /// Likelihood-ratio calibration for a batch of similarities.
    /// Mirrors `calibrated_probabilities`. The caller pre-fits the
    /// [`VectorProbabilityTransform`]; the shim here just maps cosine
    /// similarities to distances (`1 - sim`) and forwards them.
    pub fn calibrated_probabilities(
        similarities: &[f64],
        calibrator: &VectorProbabilityTransform,
        weights: Option<&[f64]>,
    ) -> Vec<f64> {
        let distances: Vec<f64> = similarities.iter().map(|s| 1.0 - *s).collect();
        calibrator.calibrate(&distances, weights)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_identity_is_one() {
        let v: Vec<f32> = vec![1.0, 0.5, -0.3];
        let c = VectorScorer::cosine_similarity(&v, &v);
        assert!((c - 1.0).abs() < 1e-9);
    }

    #[test]
    fn similarity_to_probability_maps_unit() {
        assert!((VectorScorer::similarity_to_probability(1.0) - 1.0).abs() < 1e-9);
        assert!((VectorScorer::similarity_to_probability(0.0) - 0.5).abs() < 1e-9);
        assert!((VectorScorer::similarity_to_probability(-1.0)).abs() < 1e-9);
    }

    #[test]
    fn cosine_zero_vectors_return_zero() {
        let v: Vec<f32> = vec![0.0, 0.0];
        let c = VectorScorer::cosine_similarity(&v, &v);
        assert_eq!(c, 0.0);
    }
}
