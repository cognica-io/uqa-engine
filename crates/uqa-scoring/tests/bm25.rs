//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! BM25 algebraic property tests (Paper 3, Section 3).
//!
//! Pins the four invariants Theorem 3.2.2 / 3.2.3 promise: monotone in
//! term frequency, monotone in document length, supremum at
//! `boost * IDF(df)`, and IDF non-negative whenever the collection is
//! non-trivial. Property-tested over randomly drawn `(N, df, tf, dl,
//! avgdl)` tuples so a regression in any of those properties surfaces
//! without us needing to enumerate concrete cases.

use std::sync::Arc;

use proptest::prelude::*;
use uqa_core::IndexStats;
use uqa_scoring::{BM25Params, BM25Scorer};

fn stats(n: u64, avgdl: f64) -> Arc<IndexStats> {
    let mut s = IndexStats::default();
    s.total_docs = n;
    s.avg_doc_length = avgdl;
    Arc::new(s)
}

/// Generates `(N, df, tf, dl, avgdl)` tuples where `df <= N`, `df >= 1`
/// (the rare-term path), and the document length is at most a few
/// times the average. Keeps the search space tight enough that every
/// failure has a small reproduction.
fn ranked_input() -> impl Strategy<Value = (u64, u64, u64, u64, f64)> {
    (1u64..=10_000, 1u64..=10_000, 0u64..=2_000, 1u64..=10_000).prop_flat_map(
        |(n, df_seed, tf, dl)| {
            let df = (df_seed % n).saturating_add(1).min(n);
            let avgdl = (dl as f64 * 0.5).max(1.0);
            Just((n, df, tf, dl, avgdl))
        },
    )
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        ..ProptestConfig::default()
    })]

    /// IDF is non-negative for every `df in [1, N]`. Robertson-Sparck-
    /// Jones IDF can in principle go negative when a term appears in
    /// more than half the corpus *without* the `+1` smoothing; the
    /// `ln((..)/(..) + 1)` form we use is provably non-negative.
    #[test]
    fn idf_non_negative((n, df, _tf, _dl, avgdl) in ranked_input()) {
        let bm = BM25Scorer::new(BM25Params::default(), stats(n, avgdl));
        let v = bm.idf(df);
        prop_assert!(v >= 0.0, "idf({df}) on N={n} returned negative {v}");
    }

    /// Score is non-negative for every legal input.
    #[test]
    fn score_non_negative((n, df, tf, dl, avgdl) in ranked_input()) {
        let bm = BM25Scorer::new(BM25Params::default(), stats(n, avgdl));
        let s = bm.score(tf, dl, df);
        prop_assert!(s >= 0.0, "score returned negative {s}");
    }

    /// Theorem 3.2.3: score is strictly bounded above by `boost * IDF`.
    /// We allow a small slack for f64 round-off.
    #[test]
    fn score_below_supremum((n, df, tf, dl, avgdl) in ranked_input()) {
        let bm = BM25Scorer::new(BM25Params::default(), stats(n, avgdl));
        let bound = bm.upper_bound(df);
        let s = bm.score(tf, dl, df);
        prop_assert!(
            s <= bound + 1e-9,
            "score {s} exceeded upper bound {bound} (n={n}, df={df}, tf={tf}, dl={dl})",
        );
    }

    /// Monotone in tf: doubling tf cannot decrease the score.
    /// (Strict increase is enforced by the unit test in `bm25.rs`; the
    /// property test only pins non-decrease, since two adjacent tf
    /// values can hit the same f64 bin once tf is huge.)
    #[test]
    fn score_monotone_in_tf((n, df, tf, dl, avgdl) in ranked_input()) {
        let bm = BM25Scorer::new(BM25Params::default(), stats(n, avgdl));
        let lo = bm.score(tf, dl, df);
        let hi = bm.score(tf.saturating_mul(2).max(tf + 1), dl, df);
        prop_assert!(
            hi + 1e-12 >= lo,
            "tf monotonicity broken: tf={tf} -> {lo}, 2*tf -> {hi}",
        );
    }

    /// Monotone (non-increasing) in dl for fixed tf > 0: a longer
    /// document with the same number of term occurrences cannot beat a
    /// shorter document on the same query term.
    #[test]
    fn score_non_increasing_in_dl((n, df, tf, dl, avgdl) in ranked_input()) {
        prop_assume!(tf > 0);
        let bm = BM25Scorer::new(BM25Params::default(), stats(n, avgdl));
        let short = bm.score(tf, dl, df);
        let long = bm.score(tf, dl.saturating_mul(2).max(dl + 1), df);
        prop_assert!(
            long <= short + 1e-12,
            "dl monotonicity broken: dl={dl} -> {short}, 2*dl -> {long}",
        );
    }

    /// `combine_scores` is the sum, so combining is associative and
    /// invariant under permutation of its inputs.
    #[test]
    fn combine_is_sum(scores in proptest::collection::vec(-100.0f64..100.0, 0..10)) {
        let direct: f64 = scores.iter().sum();
        let via = BM25Scorer::combine_scores(&scores);
        prop_assert!(
            (direct - via).abs() < 1e-9,
            "combine_scores diverged from sum: {via} vs {direct}",
        );
    }
}

/// Concrete supremum convergence: as tf grows the score approaches
/// the bound from below. Property-tested separately because the
/// "very large tf" knob does not fit cleanly into the random tuple
/// generator above.
#[test]
fn score_converges_to_supremum_at_large_tf() {
    let bm = BM25Scorer::new(BM25Params::default(), stats(10_000, 50.0));
    for &df in &[1u64, 50, 500, 5_000] {
        let bound = bm.upper_bound(df);
        let approached = bm.score(10_000_000, 50, df);
        assert!(
            approached <= bound + 1e-9,
            "score exceeded supremum: {approached} > {bound}",
        );
        assert!(
            approached > 0.999 * bound,
            "score did not approach supremum: {approached} vs {bound}",
        );
    }
}
