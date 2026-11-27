//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Progressive multi-signal fusion (Section 7, Paper 4).
//!
//! Each stage introduces a new batch of signals, intersects them with
//! the surviving candidate set from the prior stage, fuses every
//! accumulated signal via weighted log-odds conjunction, and keeps the
//! top-`k` highest-scoring candidates. Stages run in order; the
//! operator returns the result of the final stage.

use std::sync::Arc;

use uqa_core::{IndexStats, Payload, PostingEntry, PostingList};
use uqa_fusion::LogitGating;
use uqa_scoring::{logit, sigmoid};
use uqa_storage::StorageBackendError;

use crate::base::{ExecutionContext, Operator, OperatorResult};
use crate::hybrid::{evidence::visit_covered_scores, validate_probability_postings};

pub struct ProgressiveFusionOperator {
    pub stages: Vec<(Vec<Arc<dyn Operator>>, usize)>,
    pub alpha: f64,
    pub gating: Option<String>,
}

impl ProgressiveFusionOperator {
    pub fn new(stages: Vec<(Vec<Arc<dyn Operator>>, usize)>, alpha: f64) -> Self {
        Self::with_gating(stages, alpha, None)
    }

    pub fn with_gating(
        stages: Vec<(Vec<Arc<dyn Operator>>, usize)>,
        alpha: f64,
        gating: Option<String>,
    ) -> Self {
        Self {
            stages,
            alpha,
            gating,
        }
    }
}

impl Operator for ProgressiveFusionOperator {
    fn execute(&self, ctx: &ExecutionContext) -> OperatorResult {
        if self.stages.is_empty() {
            return Err(StorageBackendError::Other(
                "ProgressiveFusionOperator requires at least one stage".to_string(),
            ));
        }
        if !self.alpha.is_finite() || !(0.0..=1.0).contains(&self.alpha) {
            return Err(StorageBackendError::Other(format!(
                "ProgressiveFusionOperator alpha must be finite and in [0, 1], got {}",
                self.alpha
            )));
        }
        let gating = match self.gating.as_deref() {
            Some(name) => LogitGating::parse(name).ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "ProgressiveFusionOperator has unknown gating function {name:?}"
                ))
            })?,
            None => LogitGating::Pass,
        };
        let mut signal_lists: Vec<PostingList> = Vec::new();
        let mut last_result: PostingList = PostingList::new();

        for (stage, (signals, k)) in self.stages.iter().enumerate() {
            if signals.is_empty() {
                return Err(StorageBackendError::Other(
                    "ProgressiveFusionOperator stages require at least one signal".to_string(),
                ));
            }
            for signal in signals {
                let mut pl = signal.execute(ctx)?;
                validate_probability_postings(&pl, "progressive fusion")?;
                if stage > 0 {
                    let mut previous = last_result.lookup_ascending();
                    pl = PostingList::from_sorted_unchecked(
                        pl.into_iter()
                            .filter(|entry| previous(entry.doc_id).is_some())
                            .collect(),
                    );
                }
                signal_lists.push(pl);
            }
            let n = signal_lists.len() as f64;
            let confidence = n.powf(self.alpha);
            let mut scored = Vec::new();
            visit_covered_scores(&signal_lists, |doc_id, scores| {
                let mean_gated_logit = scores
                    .iter()
                    .map(|score| gating.apply(logit(*score)))
                    .sum::<f64>()
                    / n;
                scored.push(PostingEntry::new(
                    doc_id,
                    Payload::with_score(sigmoid(confidence * mean_gated_logit)),
                ));
                Ok::<_, StorageBackendError>(())
            })?;
            let scored_pl = PostingList::from_sorted_unchecked(scored);
            last_result = scored_pl.ranked().select_top_k(*k);
        }
        Ok(last_result)
    }

    fn cost_estimate(&self, stats: &IndexStats) -> f64 {
        let total_n = stats.total_docs as f64;
        let mut total = 0.0;
        let mut card = total_n;
        for (signals, k) in &self.stages {
            let ratio = if total_n > 0.0 { card / total_n } else { 1.0 };
            for sig in signals {
                total += sig.cost_estimate(stats) * ratio;
            }
            card = card.min(*k as f64);
        }
        total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ConstOperator(Vec<PostingEntry>);

    impl Operator for ConstOperator {
        fn execute(&self, _ctx: &ExecutionContext) -> OperatorResult {
            Ok(PostingList::from_sorted_unchecked(self.0.clone()))
        }
    }

    fn entry(id: u64, score: f64) -> PostingEntry {
        PostingEntry::new(id, Payload::with_score(score))
    }

    #[test]
    fn single_stage_keeps_top_k() {
        let signal = Arc::new(ConstOperator(vec![
            entry(1, 0.9),
            entry(2, 0.4),
            entry(3, 0.7),
        ])) as Arc<dyn Operator>;
        let op = ProgressiveFusionOperator::new(vec![(vec![signal], 2)], 0.0);
        let result = op.execute(&ExecutionContext::new()).unwrap();
        let ids: Vec<u64> = result.doc_ids().collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&1));
        assert!(ids.contains(&3));
    }

    #[test]
    fn second_stage_intersects_with_prior_candidates() {
        let stage_0 = Arc::new(ConstOperator(vec![
            entry(1, 0.9),
            entry(2, 0.8),
            entry(3, 0.7),
            entry(4, 0.6),
        ])) as Arc<dyn Operator>;
        let stage_1 = Arc::new(ConstOperator(vec![
            entry(1, 0.95),
            entry(4, 0.95),
            entry(5, 0.95),
        ])) as Arc<dyn Operator>;
        let op = ProgressiveFusionOperator::new(vec![(vec![stage_0], 3), (vec![stage_1], 2)], 0.0);
        let result = op.execute(&ExecutionContext::new()).unwrap();
        // Only document 1 matched the second signal inside the first cutoff.
        let miss = crate::hybrid::coverage_based_default(1, 4, 0.01);
        let expected = [(1, 0.9, 0.95), (2, 0.8, miss)]
            .map(|(id, a, b)| (id, sigmoid(f64::midpoint(logit(a), logit(b))).to_bits()));
        assert!(result
            .iter()
            .map(|entry| (entry.doc_id, entry.payload.score.to_bits()))
            .eq(expected));
    }
}
