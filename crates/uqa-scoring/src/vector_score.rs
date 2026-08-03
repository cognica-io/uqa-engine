//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Vector similarity to probability conversion.
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
//! [`VectorScorer`] exposes associated functions and carries no instance state.

use crate::calibration::VectorProbabilityTransform;
use crate::error::{invalid_input, require_finite};
use crate::{ScoringError, ScoringResult};

pub struct VectorScorer;

impl VectorScorer {
    /// Cosine similarity between two equal-length vectors. Returns 0
    /// when either side has zero norm.
    pub fn cosine_similarity(a: &[f32], b: &[f32]) -> ScoringResult<f64> {
        if a.len() != b.len() {
            return Err(invalid_input(format!(
                "vector dimensions differ: {} versus {}",
                a.len(),
                b.len()
            )));
        }
        if a.is_empty() {
            return Err(invalid_input("vectors must not be empty"));
        }
        let mut dot = 0.0_f64;
        let mut na = 0.0_f64;
        let mut nb = 0.0_f64;
        for (index, (x, y)) in a.iter().zip(b.iter()).enumerate() {
            let x = f64::from(*x);
            let y = f64::from(*y);
            require_finite(x, &format!("left vector component {index}"))?;
            require_finite(y, &format!("right vector component {index}"))?;
            dot += x * y;
            na += x * x;
            nb += y * y;
            if !dot.is_finite() || !na.is_finite() || !nb.is_finite() {
                return Err(ScoringError::ArithmeticOverflow(
                    "cosine similarity accumulation is not finite".to_string(),
                ));
            }
        }
        let denom = na.sqrt() * nb.sqrt();
        if denom == 0.0 {
            Ok(0.0)
        } else {
            let score = dot / denom;
            if !score.is_finite() || !(-1.0 - 1e-12..=1.0 + 1e-12).contains(&score) {
                return Err(ScoringError::ArithmeticOverflow(format!(
                    "cosine similarity is outside its numeric range: {score}"
                )));
            }
            Ok(score.clamp(-1.0, 1.0))
        }
    }

    /// `P_vector = (1 + score) / 2` -- Definition 7.1.2.
    pub fn similarity_to_probability(cosine_sim: f64) -> ScoringResult<f64> {
        require_finite(cosine_sim, "cosine similarity")?;
        if !(-1.0..=1.0).contains(&cosine_sim) {
            return Err(invalid_input(format!(
                "cosine similarity must be in [-1, 1], got {cosine_sim}"
            )));
        }
        Ok(f64::midpoint(1.0 + cosine_sim, 0.0))
    }

    /// Likelihood-ratio calibration for a batch of similarities. The caller
    /// pre-fits the [`VectorProbabilityTransform`]; this method maps cosine
    /// similarities to distances (`1 - sim`) and forwards them.
    pub fn calibrated_probabilities(
        similarities: &[f64],
        calibrator: &VectorProbabilityTransform,
        weights: Option<&[f64]>,
    ) -> ScoringResult<Vec<f64>> {
        let mut distances = Vec::with_capacity(similarities.len());
        for (index, similarity) in similarities.iter().copied().enumerate() {
            require_finite(similarity, &format!("similarities[{index}]"))?;
            if !(-1.0..=1.0).contains(&similarity) {
                return Err(invalid_input(format!(
                    "similarities[{index}] must be in [-1, 1], got {similarity}"
                )));
            }
            distances.push(1.0 - similarity);
        }
        calibrator.calibrate(&distances, weights)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_identity_is_one() {
        let v: Vec<f32> = vec![1.0, 0.5, -0.3];
        let c = VectorScorer::cosine_similarity(&v, &v).unwrap();
        assert!((c - 1.0).abs() < 1e-9);
    }

    #[test]
    fn similarity_to_probability_maps_unit() {
        assert!((VectorScorer::similarity_to_probability(1.0).unwrap() - 1.0).abs() < 1e-9);
        assert!((VectorScorer::similarity_to_probability(0.0).unwrap() - 0.5).abs() < 1e-9);
        assert!((VectorScorer::similarity_to_probability(-1.0).unwrap()).abs() < 1e-9);
    }

    #[test]
    fn cosine_zero_vectors_return_zero() {
        let v: Vec<f32> = vec![0.0, 0.0];
        let c = VectorScorer::cosine_similarity(&v, &v).unwrap();
        assert_eq!(c, 0.0);
    }

    #[test]
    fn invalid_vectors_are_errors() {
        assert!(VectorScorer::cosine_similarity(&[1.0], &[1.0, 2.0]).is_err());
        assert!(VectorScorer::cosine_similarity(&[f32::NAN], &[1.0]).is_err());
        assert!(VectorScorer::similarity_to_probability(1.1).is_err());
    }
}
