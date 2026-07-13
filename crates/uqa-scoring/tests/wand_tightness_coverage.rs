//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coverage for `test_wand_tightness`.

use std::collections::BTreeMap;
use std::sync::Arc;

use uqa_core::{Payload, PostingEntry, PostingList};
use uqa_scoring::{AdaptiveWANDScorer, BoundTightnessAnalyzer, Scorer, TightenedFusionWANDScorer};

struct MockScorer {
    score: f64,
}

impl Scorer for MockScorer {
    fn idf(&self, _doc_freq: u64) -> f64 {
        self.score
    }

    fn term_score(&self, _term_freq: u64, _doc_length: u64, _doc_freq: u64) -> f64 {
        self.score
    }

    fn term_score_with_idf(&self, _term_freq: u64, _doc_length: u64, _idf_val: f64) -> f64 {
        self.score
    }

    fn finalize_score(&self, scores: &[f64]) -> f64 {
        scores.iter().sum()
    }

    fn term_upper_bound(&self, _doc_freq: u64) -> f64 {
        self.score * 2.0
    }
}

fn mock(score: f64) -> Arc<dyn Scorer> {
    Arc::new(MockScorer { score })
}

fn pl(entries: &[(u64, f64)]) -> PostingList {
    PostingList::from_unsorted(
        entries
            .iter()
            .map(|(doc_id, score)| PostingEntry::new(*doc_id, Payload::with_score(*score)))
            .collect(),
    )
}

fn map(entries: &[(u64, f64)]) -> BTreeMap<u64, f64> {
    entries.iter().copied().collect()
}

fn approx_eq(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-9, "expected {a} ~= {b}");
}

#[test]
fn bound_tightness_analyzer_empty() {
    assert_eq!(BoundTightnessAnalyzer::default().tightness_ratio(), 1.0);
}

#[test]
fn bound_tightness_analyzer_perfect() {
    let mut analyzer = BoundTightnessAnalyzer::default();
    analyzer.record(5.0, 5.0);
    analyzer.record(3.0, 3.0);
    approx_eq(analyzer.tightness_ratio(), 1.0);
}

#[test]
fn bound_tightness_analyzer_loose() {
    let mut analyzer = BoundTightnessAnalyzer::default();
    analyzer.record(10.0, 5.0);
    approx_eq(analyzer.tightness_ratio(), 0.5);
}

#[test]
fn bound_tightness_slack() {
    let mut analyzer = BoundTightnessAnalyzer::default();
    analyzer.record(10.0, 5.0);
    approx_eq(analyzer.slack(), 0.5);
    analyzer.clear();
    analyzer.record(4.0, 4.0);
    approx_eq(analyzer.slack(), 0.0);
}

#[test]
fn bound_tightness_worst_index() {
    let mut analyzer = BoundTightnessAnalyzer::default();
    analyzer.record(10.0, 9.0);
    analyzer.record(10.0, 2.0);
    analyzer.record(10.0, 7.0);
    assert_eq!(analyzer.worst_bound_index(), 1);
}

#[test]
fn bound_tightness_worst_index_empty() {
    assert_eq!(BoundTightnessAnalyzer::default().worst_bound_index(), 0);
}

#[test]
fn bound_tightness_zero_upper_bound() {
    let mut analyzer = BoundTightnessAnalyzer::default();
    analyzer.record(0.0, 0.0);
    approx_eq(analyzer.tightness_ratio(), 1.0);
}

#[test]
fn bound_tightness_clear() {
    let mut analyzer = BoundTightnessAnalyzer::default();
    analyzer.record(10.0, 5.0);
    approx_eq(analyzer.tightness_ratio(), 0.5);
    analyzer.clear();
    approx_eq(analyzer.tightness_ratio(), 1.0);
}

#[test]
fn adaptive_wand_tightening() {
    let adaptive = AdaptiveWANDScorer::new(
        vec![mock(1.0), mock(2.0)],
        2,
        vec![pl(&[(1, 0.8), (2, 0.6)]), pl(&[(1, 0.9), (3, 0.5)])],
        0.8,
    );
    let bounds = adaptive.compute_upper_bounds();
    approx_eq(bounds[0], 1.6);
    approx_eq(bounds[1], 3.2);
}

#[test]
fn adaptive_wand_produces_results() {
    let mut adaptive = AdaptiveWANDScorer::new(
        vec![mock(1.0), mock(0.5)],
        2,
        vec![
            pl(&[(1, 0.9), (2, 0.7), (3, 0.5)]),
            pl(&[(1, 0.8), (2, 0.6), (4, 0.3)]),
        ],
        0.9,
    );
    let result = adaptive.score_top_k();
    assert!(result.len() <= 2);
    assert!(!result.is_empty());
}

#[test]
fn adaptive_wand_analyzer_populated() {
    let mut adaptive =
        AdaptiveWANDScorer::new(vec![mock(1.0)], 2, vec![pl(&[(1, 0.5), (2, 0.8)])], 0.9);
    adaptive.score_top_k();
    approx_eq(adaptive.analyzer.tightness_ratio(), 0.4);
}

#[test]
fn tightened_fusion_wand() {
    let mut scorer = TightenedFusionWANDScorer::new(
        vec![
            map(&[(1, 0.9), (2, 0.7), (3, 0.5)]),
            map(&[(1, 0.8), (2, 0.6), (4, 0.4)]),
        ],
        vec![0.95, 0.85],
        0.5,
        2,
        0.9,
    );
    let result = scorer.score_top_k();
    assert!(result.len() <= 2);
    assert!(!result.is_empty());
}

#[test]
fn tightened_fusion_analyzer() {
    let mut scorer = TightenedFusionWANDScorer::new(
        vec![map(&[(1, 0.9), (2, 0.7)]), map(&[(1, 0.8), (3, 0.4)])],
        vec![1.0, 1.0],
        0.5,
        2,
        0.85,
    );
    scorer.score_top_k();
    approx_eq(scorer.analyzer.tightness_ratio(), 0.85);
}

#[test]
fn tightened_fusion_preserves_original_bounds() {
    let scorer = TightenedFusionWANDScorer::new(vec![map(&[(1, 0.9)])], vec![1.0], 0.5, 1, 0.8);
    assert_eq!(scorer.original_bounds, vec![1.0]);
    assert_eq!(scorer.signal_upper_bounds, vec![0.8]);
}
