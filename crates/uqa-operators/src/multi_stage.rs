//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Multi-stage retrieval pipeline (Section 9, Paper 4).
//!
//! Each stage is `(operator, cutoff)`. Stage 0 runs fully against
//! the context; subsequent stages re-score only the candidates that
//! survived the prior cutoff. A `Cutoff::TopK(k)` keeps the `k`
//! highest-scoring entries; `Cutoff::Threshold(t)` drops entries
//! whose score is below `t`.

use std::sync::Arc;

use uqa_core::{IndexStats, PostingList};
use uqa_storage::{StorageBackendError, StorageBackendResult};

use crate::base::{require_finite_score, ExecutionContext, Operator, OperatorResult};

#[derive(Debug, Clone, Copy)]
pub enum Cutoff {
    /// Keep the top-`k` entries by score.
    TopK(usize),
    /// Keep entries whose score is `>= threshold`.
    Threshold(f64),
}

pub struct MultiStageOperator {
    pub stages: Vec<(Arc<dyn Operator>, Cutoff)>,
}

impl MultiStageOperator {
    pub fn new(stages: Vec<(Arc<dyn Operator>, Cutoff)>) -> StorageBackendResult<Self> {
        if stages.is_empty() {
            return Err(StorageBackendError::Other(
                "MultiStageOperator requires at least one stage".to_string(),
            ));
        }
        for (_, cutoff) in &stages {
            if let Cutoff::Threshold(threshold) = cutoff {
                if !threshold.is_finite() {
                    return Err(StorageBackendError::Other(
                        "MultiStageOperator thresholds must be finite".to_string(),
                    ));
                }
            }
        }
        Ok(Self { stages })
    }

    fn apply_cutoff(pl: PostingList, cutoff: Cutoff) -> StorageBackendResult<PostingList> {
        for entry in pl.entries() {
            require_finite_score(entry.payload.score, "multi-stage retrieval")?;
        }
        Ok(match cutoff {
            Cutoff::TopK(k) => pl.ranked().select_top_k(k),
            Cutoff::Threshold(t) => {
                let kept = pl.into_iter().filter(|e| e.payload.score >= t).collect();
                PostingList::from_sorted_unchecked(kept)
            }
        })
    }
}

impl Operator for MultiStageOperator {
    fn execute(&self, ctx: &ExecutionContext) -> OperatorResult {
        let (first_op, first_cutoff) = self.stages.first().ok_or_else(|| {
            StorageBackendError::Other("MultiStageOperator requires at least one stage".to_string())
        })?;
        let mut candidates = Self::apply_cutoff(first_op.execute(ctx)?, *first_cutoff)?;

        for (stage_op, cutoff) in self.stages.iter().skip(1) {
            let stage_result = stage_op.execute(ctx)?;
            for entry in stage_result.entries() {
                require_finite_score(entry.payload.score, "multi-stage retrieval")?;
            }
            let mut lookup = stage_result.lookup_ascending();
            let rescored = candidates
                .into_iter()
                .map(|mut entry| {
                    if let Some(stage) = lookup(entry.doc_id) {
                        entry.payload.score = stage.payload.score;
                    }
                    entry
                })
                .collect();
            candidates = Self::apply_cutoff(PostingList::from_sorted_unchecked(rescored), *cutoff)?;
        }
        Ok(candidates)
    }

    fn cost_estimate(&self, stats: &IndexStats) -> f64 {
        let total_n = stats.total_docs as f64;
        let mut total = 0.0;
        let mut card = total_n;
        for (op, cutoff) in &self.stages {
            let ratio = if total_n > 0.0 { card / total_n } else { 1.0 };
            total += op.cost_estimate(stats) * ratio;
            card = match cutoff {
                Cutoff::TopK(k) => card.min(*k as f64),
                // Threshold heuristic: assume half survive.
                Cutoff::Threshold(_) => card * 0.5,
            };
        }
        total
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::{Payload, PostingEntry};

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
    fn topk_cutoff_retains_only_highest_scoring() {
        let stage_0 = Arc::new(ConstOperator(vec![
            entry(1, 0.1),
            entry(2, 0.9),
            entry(3, 0.5),
        ])) as Arc<dyn Operator>;
        let pipeline = MultiStageOperator::new(vec![(stage_0, Cutoff::TopK(2))]).unwrap();
        let ctx = ExecutionContext::new();
        let out = pipeline.execute(&ctx).unwrap();
        let ids: Vec<u64> = out.doc_ids().collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&2));
        assert!(ids.contains(&3));
    }

    #[test]
    fn threshold_cutoff_drops_low_scores() {
        let op = Arc::new(ConstOperator(vec![
            entry(1, 0.2),
            entry(2, 0.5),
            entry(3, 0.8),
        ])) as Arc<dyn Operator>;
        let pipeline = MultiStageOperator::new(vec![(op, Cutoff::Threshold(0.5))]).unwrap();
        let out = pipeline.execute(&ExecutionContext::new()).unwrap();
        let ids: Vec<u64> = out.doc_ids().collect();
        assert_eq!(ids, vec![2, 3]);
    }

    #[test]
    fn second_stage_rescores_survivors_only() {
        let mut candidate = entry(3, 0.6);
        candidate.payload.positions = vec![7, 9];
        let stage_0 = Arc::new(ConstOperator(vec![
            entry(1, 0.4),
            entry(2, 0.9),
            candidate,
            entry(4, 0.1),
        ])) as Arc<dyn Operator>;
        let stage_1 = Arc::new(ConstOperator(vec![
            entry(2, 0.2),
            entry(3, 0.95),
            entry(4, 0.99),
        ])) as Arc<dyn Operator>;
        let pipeline =
            MultiStageOperator::new(vec![(stage_0, Cutoff::TopK(3)), (stage_1, Cutoff::TopK(2))])
                .unwrap();
        let out = pipeline.execute(&ExecutionContext::new()).unwrap();
        let pairs: Vec<(u64, f64)> = out
            .entries()
            .iter()
            .map(|e| (e.doc_id, e.payload.score))
            .collect();
        // Survivors: {1, 2, 3}. Stage 1 rescores 2 -> 0.2, 3 -> 0.95;
        // 1 has no stage-1 score so it carries 0.4. Top-2 by score
        // gives {3, 1}. Result returns to doc_id order: {1, 3}.
        assert_eq!(pairs, [(1, 0.4), (3, 0.95)]);
        assert_eq!(out.get_entry(3).unwrap().payload.positions, [7, 9]);
    }
}
