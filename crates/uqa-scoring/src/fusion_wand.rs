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

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};

use crate::error::{invalid_input, require_probability};
use crate::prob::confidence_scaled_log_odds_pool;
use crate::wand::BoundTightnessAnalyzer;
use crate::ScoringResult;

/// Per-signal score map keyed by document identifier.
pub type SignalScoreMap = BTreeMap<u64, f64>;

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
        validate_fusion_inputs(&self.signals, &self.upper_bounds, self.alpha)?;
        if self.signals.is_empty() {
            return Ok(Vec::new());
        }
        let mut all_doc_ids: BTreeSet<u64> = BTreeSet::new();
        for sig in &self.signals {
            all_doc_ids.extend(sig.keys().copied());
        }
        if all_doc_ids.is_empty() {
            return Ok(Vec::new());
        }
        let num_docs = all_doc_ids.len();
        let defaults: Vec<f64> = self
            .signals
            .iter()
            .map(|sig| coverage_based_default(sig.len(), num_docs, 0.01))
            .collect();

        // Min-heap keyed by score: BinaryHeap is max-heap by default,
        // so we negate via Reverse-style ordering with a wrapper.
        let mut heap: BinaryHeap<HeapEntry> = BinaryHeap::with_capacity(self.k + 1);

        for doc_id in all_doc_ids {
            // Pivot check: bail out when even the most-optimistic fused
            // upper bound for this doc cannot beat the threshold.
            if heap.len() >= self.k {
                let threshold = heap.peek().map_or(f64::NEG_INFINITY, |e| e.score);
                let doc_ubs: Vec<f64> = self
                    .signals
                    .iter()
                    .enumerate()
                    .map(|(j, sig)| {
                        if sig.contains_key(&doc_id) {
                            self.upper_bounds[j]
                        } else {
                            defaults[j]
                        }
                    })
                    .collect();
                let fused_ub = self.compute_fused_upper_bound(&doc_ubs)?;
                if fused_ub < threshold {
                    continue;
                }
            }

            // Score the document.
            let probs: Vec<f64> = self
                .signals
                .iter()
                .enumerate()
                .map(|(j, sig)| sig.get(&doc_id).copied().unwrap_or(defaults[j]))
                .collect();
            let fused = if probs.len() == 1 {
                probs[0]
            } else {
                confidence_scaled_log_odds_pool(&probs, self.alpha)
            };
            require_probability(fused, "confidence-scaled pool WAND score")?;

            if heap.len() < self.k {
                heap.push(HeapEntry {
                    score: fused,
                    doc_id,
                });
            } else if let Some(top) = heap.peek() {
                if fused > top.score {
                    heap.pop();
                    heap.push(HeapEntry {
                        score: fused,
                        doc_id,
                    });
                }
            }
        }

        // The Ord impl reverses score order so BinaryHeap acts as a
        // min-heap by score; into_sorted_vec then yields entries
        // sorted ascending in Ord order, which is descending in raw
        // score order -- exactly what callers expect from a Top-K
        // loop.
        Ok(heap
            .into_sorted_vec()
            .into_iter()
            .map(|e| (e.doc_id, e.score))
            .collect())
    }

    fn compute_fused_upper_bound(&self, active_ubs: &[f64]) -> ScoringResult<f64> {
        let bound = if active_ubs.is_empty() {
            0.0
        } else {
            confidence_scaled_log_odds_pool(active_ubs, self.alpha)
        };
        require_probability(bound, "confidence-scaled pool WAND upper bound")?;
        Ok(bound)
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

/// Score-keyed heap entry. The heap is a max-heap so the smallest
/// element after `score_top_k`'s loop is the cutoff threshold. Ties
/// resolve by doc id for determinism.
#[derive(Debug)]
struct HeapEntry {
    score: f64,
    doc_id: u64,
}

impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for HeapEntry {}

impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse ordering on score so BinaryHeap acts as a min-heap
        // over scores -- we evict the smallest-scoring entry when the
        // heap fills up.
        other
            .score
            .total_cmp(&self.score)
            .then(self.doc_id.cmp(&other.doc_id))
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

fn coverage_based_default(n_hits: usize, n_total: usize, floor: f64) -> f64 {
    if n_total == 0 {
        return 0.5;
    }
    let r = n_hits as f64 / n_total as f64;
    f64::midpoint(1.0 - r, 0.0) + floor * r
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
        assert!(s.score_top_k().unwrap().is_empty());
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
}
