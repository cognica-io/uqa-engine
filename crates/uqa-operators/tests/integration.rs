//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Consolidated operator integration tests.

#[path = "adaptive_fusion_coverage.rs"]
mod adaptive_fusion_coverage;
#[path = "attention_fusion_coverage.rs"]
mod attention_fusion_coverage;
#[path = "fusion_wand_coverage.rs"]
mod fusion_wand_coverage;
#[path = "hierarchical.rs"]
mod hierarchical;
#[path = "missing_backends.rs"]
mod missing_backends;
#[path = "multi_field.rs"]
mod multi_field;
#[path = "multi_stage_coverage.rs"]
mod multi_stage_coverage;
#[path = "progressive_fusion_coverage.rs"]
mod progressive_fusion_coverage;
#[path = "sparse_threshold_coverage.rs"]
mod sparse_threshold_coverage;
#[path = "term_synonym_union.rs"]
mod term_synonym_union;
