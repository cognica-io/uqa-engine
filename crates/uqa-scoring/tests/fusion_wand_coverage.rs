//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Confidence-scaled pool WAND scoring coverage.

use std::collections::BTreeMap;

use uqa_scoring::ConfidenceScaledPoolWANDScorer;

fn map(entries: &[(u64, f64)]) -> BTreeMap<u64, f64> {
    entries.iter().copied().collect()
}

#[test]
fn test_basic_top_k() {
    let scorer = ConfidenceScaledPoolWANDScorer::new(
        vec![
            map(&[(1, 0.9), (2, 0.7), (3, 0.5)]),
            map(&[(1, 0.8), (2, 0.6), (4, 0.4)]),
        ],
        vec![0.9, 0.8],
        0.5,
        2,
    )
    .unwrap();
    assert_eq!(scorer.score_top_k().unwrap().len(), 2);
}

#[test]
fn test_top_k_returns_highest() {
    let scorer = ConfidenceScaledPoolWANDScorer::new(
        vec![
            map(&[(1, 0.9), (2, 0.3), (3, 0.1)]),
            map(&[(1, 0.8), (2, 0.2), (3, 0.1)]),
        ],
        vec![0.9, 0.8],
        0.5,
        1,
    )
    .unwrap();
    let result = scorer.score_top_k().unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].0, 1);
}

#[test]
fn test_top_k_larger_than_docs() {
    let scorer = ConfidenceScaledPoolWANDScorer::new(
        vec![map(&[(1, 0.7)]), map(&[(1, 0.6)])],
        vec![0.7, 0.6],
        0.5,
        10,
    )
    .unwrap();
    assert_eq!(scorer.score_top_k().unwrap().len(), 1);
}

#[test]
fn test_empty_signals() {
    let scorer = ConfidenceScaledPoolWANDScorer::new(Vec::new(), Vec::new(), 0.5, 5).unwrap();
    assert!(scorer.score_top_k().unwrap().is_empty());
}

#[test]
fn test_single_signal() {
    let scorer =
        ConfidenceScaledPoolWANDScorer::new(vec![map(&[(1, 0.9), (2, 0.3)])], vec![0.9], 0.5, 1)
            .unwrap();
    let result = scorer.score_top_k().unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].0, 1);
}

#[test]
fn test_fused_upper_bound_effectively_probability() {
    let scorer = ConfidenceScaledPoolWANDScorer::new(
        vec![map(&[(1, 0.9)]), map(&[(1, 0.8)])],
        vec![0.9, 0.8],
        0.5,
        1,
    )
    .unwrap();
    let result = scorer.score_top_k().unwrap();
    assert!(result[0].1 > 0.0 && result[0].1 < 1.0);
}

#[test]
fn test_scores_are_probabilities() {
    let scorer = ConfidenceScaledPoolWANDScorer::new(
        vec![map(&[(1, 0.7), (2, 0.6)]), map(&[(1, 0.8), (2, 0.5)])],
        vec![0.8, 0.8],
        0.5,
        5,
    )
    .unwrap();
    for (_, score) in scorer.score_top_k().unwrap() {
        assert!(score > 0.0 && score < 1.0);
    }
}

#[test]
fn test_alpha_parameter() {
    let s1 = ConfidenceScaledPoolWANDScorer::new(
        vec![map(&[(1, 0.7)]), map(&[(1, 0.6)])],
        vec![0.7, 0.6],
        0.1,
        5,
    )
    .unwrap();
    let s2 = ConfidenceScaledPoolWANDScorer::new(
        vec![map(&[(1, 0.7)]), map(&[(1, 0.6)])],
        vec![0.7, 0.6],
        0.9,
        5,
    )
    .unwrap();
    assert!((s1.score_top_k().unwrap()[0].1 - s2.score_top_k().unwrap()[0].1).abs() > 1e-3);
}

#[test]
fn test_wand_gating_relu() {
    let scorer = ConfidenceScaledPoolWANDScorer::new(
        vec![map(&[(1, 0.7), (2, 0.6)]), map(&[(1, 0.8), (2, 0.5)])],
        vec![0.8, 0.8],
        0.5,
        5,
    )
    .unwrap();
    assert!(!scorer.score_top_k().unwrap().is_empty());
}

#[test]
fn test_wand_gating_swish() {
    let scorer = ConfidenceScaledPoolWANDScorer::new(
        vec![map(&[(1, 0.7), (2, 0.6)]), map(&[(1, 0.8), (2, 0.5)])],
        vec![0.8, 0.8],
        0.5,
        5,
    )
    .unwrap();
    for (_, score) in scorer.score_top_k().unwrap() {
        assert!((0.0..=1.0).contains(&score));
    }
}
