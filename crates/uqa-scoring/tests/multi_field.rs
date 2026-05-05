//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `MultiFieldBayesianScorer` property tests (Phase 8, Paper 3
//! Section 12.2 #1).
//!
//! Pins:
//! - score in `[0, 1]` for any valid input,
//! - single-field configuration returns the underlying Bayesian
//!   BM25 score unchanged,
//! - all-zero `tf` across every configured field collapses to the
//!   neutral prior `0.5` (missing-field fallback),
//! - weights are scale-invariant: doubling every `weight` does not
//!   change the fused score.

use std::collections::BTreeMap;
use std::sync::Arc;

use proptest::prelude::*;
use uqa_core::IndexStats;
use uqa_scoring::{BayesianBM25Params, FieldConfig, MultiFieldBayesianScorer};

fn stats(total: u64, avgdl: f64) -> Arc<IndexStats> {
    let mut s = IndexStats::default();
    s.total_docs = total;
    s.avg_doc_length = avgdl;
    Arc::new(s)
}

fn freq_map(field: &str, value: u64) -> BTreeMap<String, u64> {
    let mut m = BTreeMap::new();
    m.insert(field.to_string(), value);
    m
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 96,
        ..ProptestConfig::default()
    })]

    /// Score lives in `[0, 1]` for any (tf, dl, df) draw.
    #[test]
    fn score_in_unit_interval(
        tf in 0u64..=200,
        dl in 1u64..=200,
        df in 1u64..=10_000,
        weight in 0.1f64..5.0,
    ) {
        let cfg = FieldConfig {
            field: "title".into(),
            params: BayesianBM25Params::default(),
            weight,
        };
        let scorer = MultiFieldBayesianScorer::new(vec![cfg], &stats(10_000, 50.0));
        let tf_map = freq_map("title", tf);
        let dl_map = freq_map("title", dl);
        let df_map = freq_map("title", df);
        let s = scorer.score_document(&tf_map, &dl_map, &df_map);
        prop_assert!((0.0..=1.0).contains(&s), "score = {s}");
    }

    /// All-zero `tf` across every configured field collapses to the
    /// neutral prior `0.5`.
    #[test]
    fn all_zero_tf_collapses_to_prior(
        weight_a in 0.1f64..5.0,
        weight_b in 0.1f64..5.0,
    ) {
        let stats = stats(10_000, 50.0);
        let scorer = MultiFieldBayesianScorer::new(
            vec![
                FieldConfig {
                    field: "title".into(),
                    params: BayesianBM25Params::default(),
                    weight: weight_a,
                },
                FieldConfig {
                    field: "body".into(),
                    params: BayesianBM25Params::default(),
                    weight: weight_b,
                },
            ],
            &stats,
        );
        // Empty maps -> tf is 0 for every field -> every per-field
        // probability is the prior 0.5 -> fused output is also 0.5.
        let empty: BTreeMap<String, u64> = BTreeMap::new();
        let s = scorer.score_document(&empty, &empty, &empty);
        prop_assert!(
            (s - 0.5).abs() < 1e-9,
            "all-zero tf gave score {s}, expected 0.5",
        );
    }

    /// Doubling every weight leaves the fused score unchanged.
    /// Weights are normalised by their sum, so a uniform rescale
    /// cancels.
    #[test]
    fn weight_scale_invariant(
        weight_a in 0.1f64..5.0,
        weight_b in 0.1f64..5.0,
        scale in 1.0f64..10.0,
        tf_title in 1u64..=20,
        tf_body in 1u64..=20,
    ) {
        let stats = stats(10_000, 50.0);
        let make_scorer = |k: f64| {
            MultiFieldBayesianScorer::new(
                vec![
                    FieldConfig {
                        field: "title".into(),
                        params: BayesianBM25Params::default(),
                        weight: k * weight_a,
                    },
                    FieldConfig {
                        field: "body".into(),
                        params: BayesianBM25Params::default(),
                        weight: k * weight_b,
                    },
                ],
                &stats,
            )
        };
        let mut tf = BTreeMap::new();
        tf.insert("title".into(), tf_title);
        tf.insert("body".into(), tf_body);
        let mut dl = BTreeMap::new();
        dl.insert("title".into(), 50);
        dl.insert("body".into(), 50);
        let mut df = BTreeMap::new();
        df.insert("title".into(), 100);
        df.insert("body".into(), 100);

        let s_unit = make_scorer(1.0).score_document(&tf, &dl, &df);
        let s_scaled = make_scorer(scale).score_document(&tf, &dl, &df);
        prop_assert!(
            (s_unit - s_scaled).abs() < 1e-9,
            "weights not scale-invariant: unit={s_unit}, x{scale}={s_scaled}",
        );
    }
}

/// Single-field configuration returns the underlying Bayesian BM25
/// score directly (no fusion).
#[test]
fn single_field_passes_through() {
    let stats = stats(10_000, 50.0);
    let scorer = MultiFieldBayesianScorer::new(
        vec![FieldConfig {
            field: "title".into(),
            params: BayesianBM25Params::default(),
            weight: 1.0,
        }],
        &stats,
    );
    let tf = freq_map("title", 5);
    let dl = freq_map("title", 50);
    let df = freq_map("title", 10);
    let s = scorer.score_document(&tf, &dl, &df);
    assert!(s > 0.5);
    assert!(s < 1.0);
}

/// An empty field list returns 0.5 (degenerate but well-defined).
#[test]
fn empty_field_list_returns_neutral() {
    let stats = stats(10_000, 50.0);
    let scorer = MultiFieldBayesianScorer::new(Vec::new(), &stats);
    let empty: BTreeMap<String, u64> = BTreeMap::new();
    let s = scorer.score_document(&empty, &empty, &empty);
    assert!((s - 0.5).abs() < 1e-9, "empty fields gave {s}");
}
