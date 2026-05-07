//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Port of fusion-core portions of Python `test_attention_fusion.py`.

use uqa_core::IndexStats;
use uqa_fusion::{AttentionFusion, LearnedFusion, QueryFeatureExtractor};

fn assert_unit(value: f64) {
    assert!((0.0..=1.0).contains(&value), "{value} outside [0, 1]");
}

#[test]
fn test_n_features() {
    let ext = QueryFeatureExtractor::new(IndexStats::default());
    assert_eq!(ext.n_features(), 6);
}

#[test]
fn test_empty_index_returns_zeros() {
    let ext = QueryFeatureExtractor::new(IndexStats::default());
    assert_eq!(ext.extract(&["hello", "world"]), [0.0; 6]);
}

#[test]
fn test_no_matching_terms() {
    let mut stats = IndexStats::default();
    stats.total_docs = 1;
    let ext = QueryFeatureExtractor::new(stats);
    let features = ext.extract(&["xyz", "qqq"]);
    assert_eq!(features[0], 0.0);
    assert_eq!(features[4], 2.0);
    assert_eq!(features[5], 0.0);
}

#[test]
fn test_matching_terms_produce_nonzero_features() {
    let mut stats = IndexStats::default();
    stats.total_docs = 3;
    stats.set_doc_freq("_default", "hello", 2);
    stats.set_doc_freq("_default", "world", 2);
    let ext = QueryFeatureExtractor::new(stats);
    let features = ext.extract(&["hello", "world"]);
    assert!(features[0] > 0.0);
    assert!(features[1] > 0.0);
    assert!(features[2] > 0.0);
    assert_eq!(features[4], 2.0);
    assert_eq!(features[5], 1.0);
}

#[test]
fn test_attention_construction() {
    let fusion = AttentionFusion::new(3, 6, 0.5);
    assert_eq!(fusion.n_signals, 3);
    assert_eq!(fusion.n_query_features, 6);
}

#[test]
fn test_attention_fuse_result_in_unit_interval() {
    let fusion = AttentionFusion::new(2, 6, 0.0);
    assert_unit(fusion.fuse(&[0.8, 0.6], &[0.0; 6]));
}

#[test]
fn test_attention_fuse_with_nonzero_features() {
    let fusion = AttentionFusion::new(2, 6, 0.0);
    assert_unit(fusion.fuse(&[0.7, 0.3], &[1.0, 2.0, 0.5, 0.1, 3.0, 0.8]));
}

#[test]
fn test_attention_state_dict_roundtrip() {
    let fusion = AttentionFusion::new(2, 6, 0.3);
    let state = fusion.state_dict();
    assert_eq!(state.n_signals, 2);
    assert_eq!(state.n_query_features, 6);
    assert!((state.alpha - 0.3).abs() < 1e-12);

    let mut loaded = AttentionFusion::new(2, 6, 0.0);
    loaded.load_state_dict(&state);
    assert_eq!(loaded.state_dict(), state);
}

#[test]
fn test_attention_fuse_three_signals() {
    let fusion = AttentionFusion::new(3, 6, 0.0);
    assert_unit(fusion.fuse(&[0.9, 0.5, 0.2], &[0.0; 6]));
}

#[test]
fn test_learned_construction() {
    let fusion = LearnedFusion::new(3, 0.4);
    assert_eq!(fusion.n_signals(), 3);
}

#[test]
fn test_learned_fuse_result_in_unit_interval() {
    let fusion = LearnedFusion::new(2, 0.0);
    assert_unit(fusion.fuse(&[0.8, 0.6]));
}

#[test]
fn test_learned_state_dict_roundtrip() {
    let fusion = LearnedFusion::new(3, 0.7);
    let state = fusion.state_dict();
    assert_eq!(state.n_signals, 3);
    assert!((state.alpha - 0.7).abs() < 1e-12);

    let mut loaded = LearnedFusion::new(3, 0.0);
    loaded.load_state_dict(&state);
    assert_eq!(loaded.state_dict(), state);
}

#[test]
fn test_learned_fuse_three_signals() {
    let fusion = LearnedFusion::new(3, 0.0);
    assert_unit(fusion.fuse(&[0.9, 0.5, 0.2]));
}
