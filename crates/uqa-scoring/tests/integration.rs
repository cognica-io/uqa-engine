//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Consolidated scoring integration tests.

#[path = "bayesian_bm25.rs"]
mod bayesian_bm25;
#[path = "bm25.rs"]
mod bm25;
#[path = "calibration_coverage.rs"]
mod calibration_coverage;
#[path = "calibration_metrics.rs"]
mod calibration_metrics;
#[path = "external_prior_coverage.rs"]
mod external_prior_coverage;
#[path = "fusion_wand_coverage.rs"]
mod fusion_wand_coverage;
#[path = "multi_field.rs"]
mod multi_field;
#[path = "multi_field_coverage.rs"]
mod multi_field_coverage;
#[path = "parameter_learning_coverage.rs"]
mod parameter_learning_coverage;
#[path = "prob_primitives.rs"]
mod prob_primitives;
#[path = "scoring_coverage.rs"]
mod scoring_coverage;
#[path = "vector_calibration_contract.rs"]
mod vector_calibration_contract;
#[path = "wand.rs"]
mod wand;
#[path = "wand_exactness.rs"]
mod wand_exactness;
#[path = "wand_tightness_coverage.rs"]
mod wand_tightness_coverage;
