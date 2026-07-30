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

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use uqa_core::{IndexStats, Payload, PostingEntry, PostingList};
use uqa_fusion::LogitGating;
use uqa_scoring::{logit, sigmoid};
use uqa_storage::StorageBackendError;

use crate::base::{require_probability, ExecutionContext, Operator, OperatorResult};
use crate::hybrid::coverage_based_default;

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
        let mut candidate_ids: Option<BTreeSet<u64>> = None;
        let mut last_result: PostingList = PostingList::new();

        for (signals, k) in &self.stages {
            if signals.is_empty() {
                return Err(StorageBackendError::Other(
                    "ProgressiveFusionOperator stages require at least one signal".to_string(),
                ));
            }
            for signal in signals {
                let mut pl = signal.execute(ctx)?;
                for entry in pl.entries() {
                    require_probability(entry.payload.score, "progressive fusion")?;
                }
                if let Some(cands) = &candidate_ids {
                    let kept: Vec<PostingEntry> = pl
                        .entries()
                        .iter()
                        .filter(|e| cands.contains(&e.doc_id))
                        .cloned()
                        .collect();
                    pl = PostingList::from_sorted_unchecked(kept);
                }
                signal_lists.push(pl);
            }
            let mut score_maps = Vec::with_capacity(signal_lists.len());
            let mut all_doc_ids = BTreeSet::new();
            for posting in &signal_lists {
                let map: BTreeMap<u64, f64> = posting
                    .entries()
                    .iter()
                    .map(|entry| {
                        all_doc_ids.insert(entry.doc_id);
                        (entry.doc_id, entry.payload.score)
                    })
                    .collect();
                score_maps.push(map);
            }
            let total = all_doc_ids.len();
            let defaults: Vec<f64> = score_maps
                .iter()
                .map(|scores| coverage_based_default(scores.len(), total, 0.01))
                .collect();
            let n = signal_lists.len();
            let confidence = (n as f64).powf(self.alpha);
            let mut scored: Vec<PostingEntry> = all_doc_ids
                .into_iter()
                .map(|doc_id| {
                    let mean_gated_logit = score_maps
                        .iter()
                        .zip(&defaults)
                        .map(|(scores, default)| {
                            gating.apply(logit(scores.get(&doc_id).copied().unwrap_or(*default)))
                        })
                        .sum::<f64>()
                        / n as f64;
                    let fused = sigmoid(confidence * mean_gated_logit);
                    PostingEntry::new(doc_id, Payload::with_score(fused))
                })
                .collect();
            scored.sort_by_key(|e| e.doc_id);
            let scored_pl = PostingList::from_sorted_unchecked(scored);
            let topk = scored_pl.top_k(*k);
            candidate_ids = Some(topk.doc_ids().collect());
            last_result = topk;
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
        let ids: Vec<u64> = result.doc_ids().collect();
        // Stage 0 keeps {1,2,3}. Stage 1 contributes only doc 1 (4, 5
        // are filtered to the candidate set from stage 0). Remaining
        // docs accumulate stage 0 score plus 0.5 prior for stage 1
        // signal. Top-2 should be doc 1 (combo of high signal + signal)
        // and one of {2, 3}.
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&1));
    }
}
