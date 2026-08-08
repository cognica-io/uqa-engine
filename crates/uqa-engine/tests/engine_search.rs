//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Consolidated retrieval, relevance, fusion, and learned-model tests.

#[path = "beir_fixture.rs"]
mod beir_fixture;
#[path = "calibration_report_coverage.rs"]
mod calibration_report_coverage;
#[path = "hybrid_search_parity.rs"]
mod hybrid_search_parity;
#[path = "multi_field_match_lowering_parity.rs"]
mod multi_field_match_lowering_parity;
#[path = "multi_field_match_padding.rs"]
mod multi_field_match_padding;
#[path = "parameter_learning_coverage.rs"]
mod parameter_learning_coverage;
#[path = "relevance.rs"]
mod relevance;
#[path = "scored_order_by_scalar_functions.rs"]
mod scored_order_by_scalar_functions;
#[path = "scoring_params.rs"]
mod scoring_params;
#[path = "search_performance_regression.rs"]
mod search_performance_regression;
#[path = "sql_attention_fusion_coverage.rs"]
mod sql_attention_fusion_coverage;
#[path = "sql_calibrated_vector_match.rs"]
mod sql_calibrated_vector_match;
#[path = "sql_deep_predict.rs"]
mod sql_deep_predict;
#[path = "sql_external_prior_coverage.rs"]
mod sql_external_prior_coverage;
#[path = "sql_facets_highlight_coverage.rs"]
mod sql_facets_highlight_coverage;
#[path = "sql_fts_index_lifecycle.rs"]
mod sql_fts_index_lifecycle;
#[path = "sql_fts_match_coverage.rs"]
mod sql_fts_match_coverage;
#[path = "sql_fusion_wand_coverage.rs"]
mod sql_fusion_wand_coverage;
#[path = "sql_match_field_validation.rs"]
mod sql_match_field_validation;
#[path = "sql_multi_field.rs"]
mod sql_multi_field;
#[path = "sql_multi_field_coverage.rs"]
mod sql_multi_field_coverage;
#[path = "sql_multi_stage_coverage.rs"]
mod sql_multi_stage_coverage;
#[path = "sql_sparse_threshold_coverage.rs"]
mod sql_sparse_threshold_coverage;
#[path = "sql_staged_retrieval.rs"]
mod sql_staged_retrieval;
#[path = "sql_text_match_scoring.rs"]
mod sql_text_match_scoring;
#[path = "sql_uqa_highlight.rs"]
mod sql_uqa_highlight;
#[path = "text_match_with_subquery.rs"]
mod text_match_with_subquery;
#[path = "text_search_parity.rs"]
mod text_search_parity;
#[path = "text_top_k_physical.rs"]
mod text_top_k_physical;
#[path = "vector_calibration_model.rs"]
mod vector_calibration_model;
