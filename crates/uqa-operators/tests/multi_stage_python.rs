//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Port of operator portions of Python `test_multi_stage.py`.

use std::sync::Arc;

use uqa_core::{IndexStats, Payload, PostingEntry, PostingList};
use uqa_operators::{Cutoff, ExecutionContext, MultiStageOperator, Operator};

struct FixedScoreOperator(Vec<(u64, f64)>);

impl Operator for FixedScoreOperator {
    fn execute(&self, _ctx: &ExecutionContext) -> PostingList {
        PostingList::from_unsorted(
            self.0
                .iter()
                .map(|(doc_id, score)| {
                    PostingEntry::new(
                        *doc_id,
                        Payload {
                            score: *score,
                            ..Payload::default()
                        },
                    )
                })
                .collect(),
        )
    }
}

fn op(entries: &[(u64, f64)]) -> Arc<dyn Operator> {
    Arc::new(FixedScoreOperator(entries.to_vec()))
}

#[test]
fn test_single_stage_top_k() {
    let pipeline =
        MultiStageOperator::new(vec![(op(&[(1, 0.9), (2, 0.5), (3, 0.3)]), Cutoff::TopK(2))]);
    assert_eq!(pipeline.execute(&ExecutionContext::new()).len(), 2);
}

#[test]
fn test_single_stage_threshold() {
    let pipeline = MultiStageOperator::new(vec![(
        op(&[(1, 0.9), (2, 0.5), (3, 0.3)]),
        Cutoff::Threshold(0.4),
    )]);
    assert_eq!(pipeline.execute(&ExecutionContext::new()).len(), 2);
}

#[test]
fn test_two_stage_pipeline() {
    let pipeline = MultiStageOperator::new(vec![
        (
            op(&[(1, 0.9), (2, 0.7), (3, 0.5), (4, 0.3)]),
            Cutoff::TopK(3),
        ),
        (op(&[(1, 0.95), (2, 0.6), (3, 0.4)]), Cutoff::TopK(2)),
    ]);
    assert_eq!(pipeline.execute(&ExecutionContext::new()).len(), 2);
}

#[test]
fn test_stage_rescoring() {
    let pipeline = MultiStageOperator::new(vec![
        (op(&[(1, 0.5), (2, 0.9)]), Cutoff::TopK(2)),
        (op(&[(1, 0.95), (2, 0.3)]), Cutoff::TopK(1)),
    ]);
    let result = pipeline.execute(&ExecutionContext::new());
    assert_eq!(result.len(), 1);
    assert_eq!(result.entries()[0].doc_id, 1);
}

#[test]
fn test_threshold_stage() {
    let pipeline = MultiStageOperator::new(vec![
        (op(&[(1, 0.9), (2, 0.5), (3, 0.1)]), Cutoff::Threshold(0.3)),
        (op(&[(1, 0.8), (2, 0.6), (3, 0.2)]), Cutoff::Threshold(0.5)),
    ]);
    assert_eq!(pipeline.execute(&ExecutionContext::new()).len(), 2);
}

#[test]
fn test_empty_after_cutoff() {
    let pipeline =
        MultiStageOperator::new(vec![(op(&[(1, 0.3), (2, 0.2)]), Cutoff::Threshold(0.5))]);
    assert_eq!(pipeline.execute(&ExecutionContext::new()).len(), 0);
}

#[test]
#[should_panic(expected = "at least one")]
fn test_requires_at_least_one_stage() {
    MultiStageOperator::new(Vec::new());
}

#[test]
fn test_cost_estimate_cascading() {
    let pipeline = MultiStageOperator::new(vec![
        (op(&[(1, 0.9)]), Cutoff::TopK(10)),
        (op(&[(1, 0.8)]), Cutoff::TopK(5)),
    ]);
    assert!(pipeline.cost_estimate(&IndexStats::new(100)) > 0.0);
}

#[test]
fn test_three_stages() {
    let stage1: Vec<(u64, f64)> = (1..8).map(|i| (i, 0.9 - i as f64 * 0.1)).collect();
    let stage2: Vec<(u64, f64)> = (1..8).map(|i| (i, 0.8 - i as f64 * 0.05)).collect();
    let pipeline = MultiStageOperator::new(vec![
        (op(&stage1), Cutoff::TopK(5)),
        (op(&stage2), Cutoff::TopK(3)),
        (op(&[(1, 0.99), (2, 0.5)]), Cutoff::TopK(1)),
    ]);
    assert_eq!(pipeline.execute(&ExecutionContext::new()).len(), 1);
}
