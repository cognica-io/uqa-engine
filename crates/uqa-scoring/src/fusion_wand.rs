//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! WAND-style top-k pruning for the confidence-scaled log-odds ranking pool.
//!
//! Treats each input posting list as a "term" in the WAND framework.
//! Per-signal upper bounds compose into a pooled upper bound through
//! [`crate::prob::confidence_scaled_log_odds_pool`], which is monotone in each
//! input probability so the bound is safe for pruning. Documents whose
//! fused upper bound cannot beat the current top-k threshold are
//! skipped without scoring.
//!
//! Inputs are pre-collected `(doc_id, score)` pairs (the upstream
//! posting lists already produced their per-signal probabilities);
//! the scorer focuses purely on the heuristic pool's pivot loop. Exact Bayesian
//! evidence fusion uses a separate operator and does not use this scorer.

use std::collections::{BTreeMap, BinaryHeap};

use crate::error::{invalid_input, require_probability};
use crate::prob::{coverage_based_default, pool_with_confidence_scale};
use crate::wand::{
    count_distinct_docs, update_top_k, BoundTightnessAnalyzer, HeapEntry, WANDStats,
};
use crate::ScoringResult;

/// Per-signal score map keyed by document identifier.
pub type SignalScoreMap = BTreeMap<u64, f64>;

const SIGNAL_WINDOW: usize = 16;

/// A bounded sorted slice of one signal. Sequential candidates consume the
/// slice directly. A pivot beyond it opens a range at the target key.
struct SignalCursor<'a> {
    map: &'a SignalScoreMap,
    input: std::collections::btree_map::Range<'a, u64, f64>,
    buffer: [(u64, f64); SIGNAL_WINDOW],
    start: usize,
    end: usize,
}

impl<'a> SignalCursor<'a> {
    fn new(map: &'a SignalScoreMap) -> Self {
        let mut cursor = Self {
            map,
            input: map.range(..),
            buffer: [(0, 0.0); SIGNAL_WINDOW],
            start: 0,
            end: 0,
        };
        cursor.refill();
        cursor
    }

    fn refill(&mut self) {
        self.start = 0;
        self.end = 0;
        for (slot, (&id, &score)) in self.buffer.iter_mut().zip(&mut self.input) {
            *slot = (id, score);
            self.end += 1;
        }
    }

    fn scores(&self) -> &[(u64, f64)] {
        &self.buffer[self.start..self.end]
    }

    fn advance(&mut self) {
        self.start += 1;
        if self.start == self.end {
            self.refill();
        }
    }

    fn seek(&mut self, target: u64) {
        self.start += self.scores().partition_point(|entry| entry.0 < target);
        if self.start == self.end {
            self.input = self.map.range(target..);
            self.refill();
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConfidenceScaledPoolWANDScorer {
    pub signals: Vec<SignalScoreMap>,
    pub upper_bounds: Vec<f64>,
    pub alpha: f64,
    pub k: usize,
}

impl ConfidenceScaledPoolWANDScorer {
    pub fn new(
        signals: Vec<SignalScoreMap>,
        upper_bounds: Vec<f64>,
        alpha: f64,
        k: usize,
    ) -> ScoringResult<Self> {
        validate_fusion_inputs(&signals, &upper_bounds, alpha)?;
        Ok(Self {
            signals,
            upper_bounds,
            alpha,
            k,
        })
    }

    /// Run the WAND pivot loop and return the top-k `(doc_id, score)` pairs
    /// sorted by descending score.
    pub fn score_top_k(&self) -> ScoringResult<Vec<(u64, f64)>> {
        let (heap, _) = self.select()?;
        Ok(heap
            .into_sorted_vec()
            .into_iter()
            .map(|entry| (entry.doc_id, entry.score))
            .collect())
    }

    fn select(&self) -> ScoringResult<(BinaryHeap<HeapEntry>, WANDStats)> {
        validate_fusion_inputs(&self.signals, &self.upper_bounds, self.alpha)?;
        let mut stats = WANDStats::default();
        if self.k == 0 || self.signals.is_empty() {
            return Ok((BinaryHeap::new(), stats));
        }
        let total = count_distinct_docs(self.signals.iter().map(|signal| signal.keys().copied()))
            .ok_or_else(|| {
            invalid_input("confidence-scaled pool candidate count overflowed")
        })?;
        stats.total_candidates = u64::try_from(total)
            .map_err(|_| invalid_input("confidence-scaled pool candidate count exceeds u64"))?;
        let mut heap = BinaryHeap::with_capacity(self.k.min(total));
        let defaults: Vec<_> = self
            .signals
            .iter()
            .map(|signal| coverage_based_default(signal.len(), total, 0.01))
            .collect();
        let mut remaining: Vec<_> = self.signals.iter().map(SignalCursor::new).collect();
        let scale = (defaults.len() as f64).powf(self.alpha);
        let score_row = |row: &[f64]| match row {
            [single] => *single,
            _ => pool_with_confidence_scale(row, scale),
        };
        let mut row = defaults.clone();
        let mut threshold = 0.0;
        while let Some(doc) = remaining
            .iter()
            .filter_map(|column| column.scores().first().map(|entry| entry.0))
            .min()
        {
            if heap.len() == self.k {
                let mut pivot = None;
                for (i, column) in remaining.iter().enumerate() {
                    row[i] = defaults[i];
                    if let Some(&(id, _)) = column.scores().first() {
                        if id == doc {
                            row[i] = if column.map.len() == total {
                                self.upper_bounds[i]
                            } else {
                                defaults[i].max(self.upper_bounds[i])
                            };
                        } else {
                            pivot = Some(pivot.map_or(id, |previous: u64| previous.min(id)));
                        }
                    }
                }
                // Until the next distinct head, only columns at doc can supply
                // a hit. Only partial-coverage signals can supply a missing score.
                if score_row(&row) < threshold {
                    let Some(pivot) = pivot else { break };
                    for column in &mut remaining {
                        if column.scores().first().is_some_and(|entry| entry.0 == doc) {
                            column.seek(pivot);
                            stats.cursor_advances += 1;
                        }
                    }
                    continue;
                }
            }
            for (i, column) in remaining.iter_mut().enumerate() {
                row[i] = defaults[i];
                if let Some(&(id, score)) = column.scores().first() {
                    if id == doc {
                        row[i] = score;
                        column.advance();
                    }
                }
            }
            let score = score_row(&row);
            require_probability(score, "confidence-scaled pool WAND score")?;
            stats.scored += 1;
            update_top_k(&mut heap, self.k, score, doc, &mut threshold);
        }
        Ok((heap, stats))
    }
}

/// Tightened variant that scales the supplied upper bounds by
/// `tightening_factor` (default 0.9) before pruning.
#[derive(Debug, Clone)]
pub struct TightenedConfidenceScaledPoolWANDScorer {
    pub inner: ConfidenceScaledPoolWANDScorer,
    pub tightening_factor: f64,
    pub original_bounds: Vec<f64>,
    pub signal_upper_bounds: Vec<f64>,
    pub analyzer: BoundTightnessAnalyzer,
}

impl TightenedConfidenceScaledPoolWANDScorer {
    #[allow(clippy::needless_pass_by_value)]
    pub fn new(
        signals: Vec<SignalScoreMap>,
        upper_bounds: Vec<f64>,
        alpha: f64,
        k: usize,
        tightening_factor: f64,
    ) -> ScoringResult<Self> {
        validate_fusion_inputs(&signals, &upper_bounds, alpha)?;
        if !tightening_factor.is_finite() || !(0.0..=1.0).contains(&tightening_factor) {
            return Err(invalid_input(format!(
                "confidence-scaled pool WAND tightening factor must be finite and in [0, 1], got {tightening_factor}"
            )));
        }
        let tightened: Vec<f64> = upper_bounds
            .iter()
            .zip(&signals)
            .map(|(upper_bound, signal)| {
                let observed_max = signal.values().copied().fold(0.0_f64, f64::max);
                (upper_bound * tightening_factor).max(observed_max)
            })
            .collect();
        let original_bounds = upper_bounds.clone();
        let signal_upper_bounds = tightened.clone();
        let inner = ConfidenceScaledPoolWANDScorer::new(signals, tightened, alpha, k)?;
        Ok(Self {
            inner,
            tightening_factor,
            original_bounds,
            signal_upper_bounds,
            analyzer: BoundTightnessAnalyzer::default(),
        })
    }

    pub fn score_top_k(&mut self) -> ScoringResult<Vec<(u64, f64)>> {
        self.analyzer.clear();
        for (idx, sig) in self.inner.signals.iter().enumerate() {
            let actual = sig.values().copied().fold(0.0_f64, f64::max);
            if let Some(bound) = self.original_bounds.get(idx) {
                self.analyzer.record(*bound, actual)?;
            }
        }
        self.inner.score_top_k()
    }
}

fn validate_fusion_inputs(
    signals: &[SignalScoreMap],
    upper_bounds: &[f64],
    alpha: f64,
) -> ScoringResult<()> {
    if signals.len() != upper_bounds.len() {
        return Err(invalid_input(format!(
            "confidence-scaled pool WAND requires one upper bound per signal, got {} signals and {} bounds",
            signals.len(),
            upper_bounds.len()
        )));
    }
    if !alpha.is_finite() || !(0.0..=1.0).contains(&alpha) {
        return Err(invalid_input(format!(
            "confidence-scaled pool WAND alpha must be finite and in [0, 1], got {alpha}"
        )));
    }
    for (signal_index, (signal, upper_bound)) in signals.iter().zip(upper_bounds).enumerate() {
        require_probability(*upper_bound, "confidence-scaled pool WAND upper bound")?;
        for (doc_id, score) in signal {
            require_probability(*score, "confidence-scaled pool WAND signal score")?;
            if score > upper_bound {
                return Err(invalid_input(format!(
                    "confidence-scaled pool WAND score {score} for document {doc_id} in signal {signal_index} exceeds upper bound {upper_bound}"
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(u64, f64)]) -> SignalScoreMap {
        pairs.iter().copied().collect()
    }

    #[test]
    fn returns_empty_for_no_signals() {
        let s = ConfidenceScaledPoolWANDScorer::new(vec![], vec![], 0.5, 10).unwrap();
        assert_eq!(s.score_top_k().unwrap().len(), 0);
    }

    #[test]
    fn fuses_two_signals_descending_score() {
        let s = ConfidenceScaledPoolWANDScorer::new(
            vec![
                map(&[(1, 0.8), (2, 0.6), (3, 0.4)]),
                map(&[(1, 0.9), (2, 0.5), (3, 0.3)]),
            ],
            vec![0.95, 0.95],
            0.5,
            3,
        )
        .unwrap();
        let result = s.score_top_k().unwrap();
        assert_eq!(result.len(), 3);
        // Descending order.
        for w in result.windows(2) {
            assert!(w[0].1 >= w[1].1, "result not sorted descending: {result:?}");
        }
        // Doc 1 should win because both signals score it highest.
        assert_eq!(result[0].0, 1);
    }

    #[test]
    fn tightening_scales_bounds() {
        let s = TightenedConfidenceScaledPoolWANDScorer::new(
            vec![map(&[(1, 0.6)]), map(&[(1, 0.6)])],
            vec![0.9, 0.9],
            0.5,
            5,
            0.5,
        )
        .unwrap();
        for ub in &s.inner.upper_bounds {
            assert!((*ub - 0.6).abs() < 1e-9);
        }
        assert_eq!(s.original_bounds, vec![0.9, 0.9]);
    }

    #[test]
    fn invalid_shapes_and_scores_are_rejected() {
        assert!(
            ConfidenceScaledPoolWANDScorer::new(vec![map(&[(1, 0.5)])], vec![], 0.5, 1).is_err()
        );
        assert!(ConfidenceScaledPoolWANDScorer::new(
            vec![map(&[(1, f64::NAN)])],
            vec![1.0],
            0.5,
            1,
        )
        .is_err());
        assert!(
            ConfidenceScaledPoolWANDScorer::new(vec![map(&[(1, 0.8)])], vec![0.7], 0.5, 1).is_err()
        );
        assert!(ConfidenceScaledPoolWANDScorer::new(
            vec![map(&[(1, 0.5)])],
            vec![1.0],
            f64::NAN,
            1
        )
        .is_err());
    }

    #[test]
    fn sparse_pivot_skips_weak_scores_without_changing_the_top_document() {
        let common: SignalScoreMap = (1..=8192).map(|doc| (doc, 0.6)).collect();
        let rare = map(&[(1, 0.99), (8192, 0.99)]);
        let scorer =
            ConfidenceScaledPoolWANDScorer::new(vec![common, rare], vec![0.6, 0.99], 0.5, 1)
                .unwrap();
        let (heap, stats) = scorer.select().unwrap();
        let winner = heap.peek().unwrap();
        assert_eq!(winner.doc_id, 1);
        assert_eq!(
            winner.score.to_bits(),
            crate::prob::confidence_scaled_log_odds_pool(&[0.6, 0.99], 0.5).to_bits()
        );
        assert_eq!(
            stats,
            WANDStats {
                total_candidates: 8192,
                scored: 2,
                cursor_advances: 1
            }
        );
    }

    #[test]
    fn full_coverage_prunes_with_the_explicit_bound_and_partial_coverage_can_still_win() {
        let dense = (1..=8192).map(|id| (id, 0.001)).collect();
        let scorer = ConfidenceScaledPoolWANDScorer::new(
            vec![dense, map(&[(1, 0.9)])],
            vec![0.001, 0.9],
            0.0,
            1,
        )
        .unwrap();
        let (heap, stats) = scorer.select().unwrap();
        let winner = heap.peek().unwrap();
        assert_eq!(winner.doc_id, 1);
        assert_eq!(
            winner.score.to_bits(),
            crate::prob::confidence_scaled_log_odds_pool(&[0.001, 0.9], 0.0).to_bits()
        );
        assert_eq!(stats.total_candidates, 8192);
        assert_eq!(stats.scored, 1);

        // Both partial signals still have a head at document 2. Document 4
        // wins because the first signal's missing score exceeds its hit bound.
        let scorer = ConfidenceScaledPoolWANDScorer::new(
            vec![
                map(&[(1, 0.001), (2, 0.001), (3, 0.001)]),
                map(&[(1, 0.6), (2, 0.6), (4, 0.6)]),
                map(&[(1, 0.8)]),
            ],
            vec![0.001, 0.6, 0.8],
            0.0,
            1,
        )
        .unwrap();
        let expected = crate::prob::confidence_scaled_log_odds_pool(
            &[
                coverage_based_default(3, 4, 0.01),
                0.6,
                coverage_based_default(1, 4, 0.01),
            ],
            0.0,
        );
        let actual = scorer.score_top_k().unwrap();
        assert_eq!(actual.len(), 1);
        assert_eq!(
            (actual[0].0, actual[0].1.to_bits()),
            (4, expected.to_bits())
        );
    }

    #[test]
    fn sparse_pool_matches_exhaustive_scores_and_ties_for_every_cutoff() {
        let columns = vec![
            map(&[(2, 0.0), (3, 0.1), (7, 0.04)]),
            map(&[(0, 1.0), (2, 0.2), (4, 0.99), (7, 0.0), (u64::MAX, 1.0)]),
            map(&[(1, 0.5), (3, 0.8), (4, 0.9), (7, 0.6)]),
        ];
        let ids: std::collections::BTreeSet<_> = columns
            .iter()
            .flat_map(|column| column.keys().copied())
            .collect();
        let defaults: Vec<_> = columns
            .iter()
            .map(|column| coverage_based_default(column.len(), ids.len(), 0.01))
            .collect();
        for alpha in [0.0, 0.5, 1.0] {
            let mut expected: Vec<_> = ids
                .iter()
                .map(|doc| {
                    let probabilities: Vec<_> = columns
                        .iter()
                        .zip(&defaults)
                        .map(|(column, default)| *column.get(doc).unwrap_or(default))
                        .collect();
                    (
                        *doc,
                        crate::prob::confidence_scaled_log_odds_pool(&probabilities, alpha),
                    )
                })
                .collect();
            expected.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            for k in (0..=ids.len() + 1).chain([usize::MAX]) {
                let scorer = ConfidenceScaledPoolWANDScorer::new(
                    columns.clone(),
                    vec![0.1, 1.0, 0.9],
                    alpha,
                    k,
                )
                .unwrap();
                let actual: Vec<_> = scorer
                    .score_top_k()
                    .unwrap()
                    .into_iter()
                    .map(|(id, score)| (id, score.to_bits()))
                    .collect();
                assert_eq!(
                    actual,
                    expected
                        .iter()
                        .take(k)
                        .map(|(id, score)| (*id, score.to_bits()))
                        .collect::<Vec<_>>()
                );
            }
        }
        let just_below_one = f64::from_bits(1.0_f64.to_bits() - 1);
        let scorer = ConfidenceScaledPoolWANDScorer::new(
            vec![map(&[(1, just_below_one), (2, 1.0)])],
            vec![1.0],
            0.5,
            1,
        )
        .unwrap();
        assert_eq!(scorer.score_top_k().unwrap(), [(2, 1.0)]);
    }

    #[test]
    fn overlapping_pool_inputs_use_bounded_scratch_independent_of_population() {
        let measure = |count| {
            let signals = (0..32)
                .map(|_| (1..=count).map(|doc| (doc, 0.5)).collect())
                .collect();
            let scorer =
                ConfidenceScaledPoolWANDScorer::new(signals, vec![0.99; 32], 0.5, 10).unwrap();
            allocation_counter::measure(|| {
                let result = scorer.score_top_k().unwrap();
                assert!(result.iter().map(|(id, _)| *id).eq(1..=10));
            })
        };
        let small = measure(32);
        let large = measure(8192);
        assert_eq!(large.bytes_max, small.bytes_max);
        assert_eq!(large.count_total, small.count_total);
        assert!(large.count_total < 32, "{large:?}");
    }
}
