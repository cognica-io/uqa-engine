//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistable vector-calibration models and their compatibility contract.
//!
//! A bare [`crate::VectorProbabilityTransform`] is only a numeric mapping.
//! Reusing one safely requires knowing the corpus, physical index, embedding
//! model, candidate-pool size, and versions it was fitted against. This module
//! keeps that provenance inseparable from a reusable model and rejects a
//! runtime target that does not match it exactly.

use serde::{Deserialize, Serialize};

use crate::error::{invalid_input, require_finite};
use crate::{ScoringError, ScoringResult, VectorProbabilityTransform};

/// JSON schema version for [`VectorCalibrationModel`].
pub const VECTOR_CALIBRATION_MODEL_SCHEMA_VERSION: u32 = 1;

/// Runtime identity of the retrieval surface to which a calibration model
/// applies. Versions are opaque, caller-controlled identifiers (for example a
/// content digest, catalog generation, or immutable release id).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VectorCalibrationTarget {
    pub corpus_id: String,
    pub corpus_version: String,
    pub index_id: String,
    pub index_version: String,
    pub index_kind: String,
    pub embedding_model_id: String,
    pub embedding_model_version: String,
    pub candidate_k: usize,
    pub dimensions: u32,
}

impl VectorCalibrationTarget {
    pub fn validate(&self) -> ScoringResult<()> {
        for (name, value) in [
            ("corpus_id", self.corpus_id.as_str()),
            ("corpus_version", self.corpus_version.as_str()),
            ("index_id", self.index_id.as_str()),
            ("index_version", self.index_version.as_str()),
            ("index_kind", self.index_kind.as_str()),
            ("embedding_model_id", self.embedding_model_id.as_str()),
            (
                "embedding_model_version",
                self.embedding_model_version.as_str(),
            ),
        ] {
            if value.trim().is_empty() {
                return Err(invalid_input(format!("{name} must not be empty")));
            }
        }
        if self.candidate_k == 0 {
            return Err(invalid_input("candidate_k must be greater than zero"));
        }
        if self.dimensions == 0 {
            return Err(invalid_input("dimensions must be greater than zero"));
        }
        Ok(())
    }
}

/// Provenance stored with a fitted vector-calibration model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VectorCalibrationProvenance {
    /// Version of the fitted parameters or training recipe.
    pub model_version: String,
    /// Exact retrieval target used for fitting.
    pub target: VectorCalibrationTarget,
    /// Number of labeled or background samples used to fit the model.
    pub fit_sample_count: usize,
}

impl VectorCalibrationProvenance {
    pub fn validate(&self) -> ScoringResult<()> {
        if self.model_version.trim().is_empty() {
            return Err(invalid_input("model_version must not be empty"));
        }
        if self.fit_sample_count < 2 {
            return Err(invalid_input(format!(
                "fit_sample_count must be at least 2, got {}",
                self.fit_sample_count
            )));
        }
        self.target.validate()
    }
}

/// A reusable vector-calibration transform with mandatory provenance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorCalibrationModel {
    schema_version: u32,
    transform: VectorProbabilityTransform,
    provenance: VectorCalibrationProvenance,
}

impl VectorCalibrationModel {
    pub fn new(
        transform: VectorProbabilityTransform,
        provenance: VectorCalibrationProvenance,
    ) -> ScoringResult<Self> {
        let model = Self {
            schema_version: VECTOR_CALIBRATION_MODEL_SCHEMA_VERSION,
            transform,
            provenance,
        };
        model.validate()?;
        Ok(model)
    }

    pub fn transform(&self) -> VectorProbabilityTransform {
        self.transform
    }

    pub fn provenance(&self) -> &VectorCalibrationProvenance {
        &self.provenance
    }

    pub fn validate_for(&self, target: &VectorCalibrationTarget) -> ScoringResult<()> {
        self.validate()?;
        target.validate()?;
        if self.provenance.target != *target {
            return Err(invalid_input(format!(
                "vector calibration target mismatch: model={:?}, runtime={target:?}",
                self.provenance.target
            )));
        }
        Ok(())
    }

    pub fn calibrate_one(
        &self,
        distance: f64,
        target: &VectorCalibrationTarget,
    ) -> ScoringResult<f64> {
        self.validate_for(target)?;
        self.transform.calibrate_one(distance)
    }

    pub fn calibrate(
        &self,
        distances: &[f64],
        target: &VectorCalibrationTarget,
    ) -> ScoringResult<Vec<f64>> {
        self.validate_for(target)?;
        self.transform.calibrate(distances, None)
    }

    pub fn to_json(&self) -> ScoringResult<String> {
        self.validate()?;
        serde_json::to_string(self)
            .map_err(|error| invalid_input(format!("serialize vector calibration model: {error}")))
    }

    pub fn from_json(json: &str) -> ScoringResult<Self> {
        let model: Self = serde_json::from_str(json).map_err(|error| {
            invalid_input(format!("deserialize vector calibration model: {error}"))
        })?;
        model.validate()?;
        Ok(model)
    }

    fn validate(&self) -> ScoringResult<()> {
        if self.schema_version != VECTOR_CALIBRATION_MODEL_SCHEMA_VERSION {
            return Err(invalid_input(format!(
                "unsupported vector calibration schema version {}, expected {}",
                self.schema_version, VECTOR_CALIBRATION_MODEL_SCHEMA_VERSION
            )));
        }
        VectorProbabilityTransform::new(
            self.transform.mu_match,
            self.transform.mu_random,
            self.transform.sigma,
            self.transform.base_rate,
        )?;
        self.provenance.validate()
    }
}

/// Probability drift observed when two calibration models score the same
/// distance probes. This makes candidate-`K` sensitivity a measured contract
/// instead of an informal observation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VectorCalibrationStabilityReport {
    pub reference_k: usize,
    pub candidate_k: usize,
    pub probe_count: usize,
    pub mean_absolute_drift: f64,
    pub max_absolute_drift: f64,
}

impl VectorCalibrationStabilityReport {
    pub fn compare(
        reference: &VectorCalibrationModel,
        candidate: &VectorCalibrationModel,
        probe_distances: &[f64],
    ) -> ScoringResult<Self> {
        if probe_distances.is_empty() {
            return Err(invalid_input(
                "calibration stability probes must not be empty",
            ));
        }
        let reference_target = &reference.provenance.target;
        let candidate_target = &candidate.provenance.target;
        let reference_probabilities = reference.calibrate(probe_distances, reference_target)?;
        let candidate_probabilities = candidate.calibrate(probe_distances, candidate_target)?;
        let mut sum = 0.0;
        let mut max = 0.0_f64;
        for (left, right) in reference_probabilities
            .iter()
            .zip(candidate_probabilities.iter())
        {
            let drift = (left - right).abs();
            require_finite(drift, "vector calibration probability drift")?;
            sum += drift;
            max = max.max(drift);
            if !sum.is_finite() {
                return Err(ScoringError::ArithmeticOverflow(
                    "vector calibration drift accumulation is not finite".into(),
                ));
            }
        }
        Ok(Self {
            reference_k: reference_target.candidate_k,
            candidate_k: candidate_target.candidate_k,
            probe_count: probe_distances.len(),
            mean_absolute_drift: sum / probe_distances.len() as f64,
            max_absolute_drift: max,
        })
    }
}
