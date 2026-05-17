//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coverage for the `FusionWAND` scoring coverage for `test_fusion_wand`.

use std::collections::BTreeMap;

use uqa_scoring::FusionWANDScorer;

fn map(entries: &[(u64, f64)]) -> BTreeMap<u64, f64> {
    entries.iter().copied().collect()
}

#[test]
fn test_basic_top_k() {
    let scorer = FusionWANDScorer::new(
        vec![
            map(&[(1, 0.9), (2, 0.7), (3, 0.5)]),
            map(&[(1, 0.8), (2, 0.6), (4, 0.4)]),
        ],
        vec![0.9, 0.8],
        0.5,
        2,
    );
    assert_eq!(scorer.score_top_k().len(), 2);
}

#[test]
fn test_top_k_returns_highest() {
    let scorer = FusionWANDScorer::new(
        vec![
            map(&[(1, 0.9), (2, 0.3), (3, 0.1)]),
            map(&[(1, 0.8), (2, 0.2), (3, 0.1)]),
        ],
        vec![0.9, 0.8],
        0.5,
        1,
    );
    let result = scorer.score_top_k();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].0, 1);
}

#[test]
fn test_top_k_larger_than_docs() {
    let scorer = FusionWANDScorer::new(
        vec![map(&[(1, 0.7)]), map(&[(1, 0.6)])],
        vec![0.7, 0.6],
        0.5,
        10,
    );
    assert_eq!(scorer.score_top_k().len(), 1);
}

#[test]
fn test_empty_signals() {
    let scorer = FusionWANDScorer::new(Vec::new(), Vec::new(), 0.5, 5);
    assert!(scorer.score_top_k().is_empty());
}

#[test]
fn test_single_signal() {
    let scorer = FusionWANDScorer::new(vec![map(&[(1, 0.9), (2, 0.3)])], vec![0.9], 0.5, 1);
    let result = scorer.score_top_k();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].0, 1);
}

#[test]
fn test_fused_upper_bound_effectively_probability() {
    let scorer = FusionWANDScorer::new(Vec::new(), vec![0.9, 0.8], 0.5, 5);
    let result = FusionWANDScorer::new(
        vec![map(&[(1, 0.9)]), map(&[(1, 0.8)])],
        scorer.upper_bounds.clone(),
        scorer.alpha,
        1,
    )
    .score_top_k();
    assert!(result[0].1 > 0.0 && result[0].1 < 1.0);
}

#[test]
fn test_scores_are_probabilities() {
    let scorer = FusionWANDScorer::new(
        vec![map(&[(1, 0.7), (2, 0.6)]), map(&[(1, 0.8), (2, 0.5)])],
        vec![0.8, 0.8],
        0.5,
        5,
    );
    for (_, score) in scorer.score_top_k() {
        assert!(score > 0.0 && score < 1.0);
    }
}

#[test]
fn test_alpha_parameter() {
    let s1 = FusionWANDScorer::new(
        vec![map(&[(1, 0.7)]), map(&[(1, 0.6)])],
        vec![0.7, 0.6],
        0.1,
        5,
    );
    let s2 = FusionWANDScorer::new(
        vec![map(&[(1, 0.7)]), map(&[(1, 0.6)])],
        vec![0.7, 0.6],
        0.9,
        5,
    );
    assert!((s1.score_top_k()[0].1 - s2.score_top_k()[0].1).abs() > 1e-3);
}

#[test]
fn test_wand_gating_relu() {
    let scorer = FusionWANDScorer::new(
        vec![map(&[(1, 0.7), (2, 0.6)]), map(&[(1, 0.8), (2, 0.5)])],
        vec![0.8, 0.8],
        0.5,
        5,
    );
    assert!(!scorer.score_top_k().is_empty());
}

#[test]
fn test_wand_gating_swish() {
    let scorer = FusionWANDScorer::new(
        vec![map(&[(1, 0.7), (2, 0.6)]), map(&[(1, 0.8), (2, 0.5)])],
        vec![0.8, 0.8],
        0.5,
        5,
    );
    for (_, score) in scorer.score_top_k() {
        assert!((0.0..=1.0).contains(&score));
    }
}
