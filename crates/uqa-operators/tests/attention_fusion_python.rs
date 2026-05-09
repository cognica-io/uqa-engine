//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Port of operator portions of Python `test_attention_fusion.py`.

use std::sync::Arc;

use uqa_core::{IndexStats, Payload, PostingEntry, PostingList};
use uqa_fusion::{AttentionFusion, LearnedFusion};
use uqa_operators::{
    AttentionFuser, AttentionFusionOperator, ExecutionContext, LearnedFusionOperator, Operator,
};

struct FixedOperator(Vec<(u64, f64)>);

impl Operator for FixedOperator {
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

fn signal(entries: &[(u64, f64)]) -> Arc<dyn Operator> {
    Arc::new(FixedOperator(entries.to_vec()))
}

#[test]
fn test_attention_empty_signals_return_empty() {
    let op = AttentionFusionOperator::new(
        vec![signal(&[]), signal(&[])],
        AttentionFuser::Single(AttentionFusion::new(2, 6, 0.0)),
        vec![0.0; 6],
    );
    assert_eq!(op.execute(&ExecutionContext::new()).len(), 0);
}

#[test]
fn test_attention_fuses_two_signals() {
    let op = AttentionFusionOperator::new(
        vec![signal(&[(1, 0.8), (2, 0.6)]), signal(&[(1, 0.7), (3, 0.5)])],
        AttentionFuser::Single(AttentionFusion::new(2, 6, 0.0)),
        vec![0.0; 6],
    );
    let result = op.execute(&ExecutionContext::new());
    let ids: Vec<u64> = result.iter().map(|entry| entry.doc_id).collect();
    assert_eq!(ids, vec![1, 2, 3]);
    for entry in &result {
        assert!(entry.payload.score > 0.0 && entry.payload.score < 1.0);
    }
}

#[test]
fn test_attention_cost_estimate() {
    let op = AttentionFusionOperator::new(
        vec![signal(&[(1, 0.8)]), signal(&[(2, 0.7)])],
        AttentionFuser::Single(AttentionFusion::new(2, 6, 0.0)),
        vec![0.0; 6],
    );
    assert!(op.cost_estimate(&IndexStats::new(100)) >= 0.0);
}

#[test]
fn test_learned_empty_signals_return_empty() {
    let op = LearnedFusionOperator::new(vec![signal(&[]), signal(&[])], LearnedFusion::new(2, 0.0));
    assert_eq!(op.execute(&ExecutionContext::new()).len(), 0);
}

#[test]
fn test_learned_fuses_two_signals() {
    let op = LearnedFusionOperator::new(
        vec![signal(&[(1, 0.8), (2, 0.6)]), signal(&[(1, 0.7), (3, 0.5)])],
        LearnedFusion::new(2, 0.0),
    );
    let result = op.execute(&ExecutionContext::new());
    let ids: Vec<u64> = result.iter().map(|entry| entry.doc_id).collect();
    assert_eq!(ids, vec![1, 2, 3]);
    for entry in &result {
        assert!(entry.payload.score > 0.0 && entry.payload.score < 1.0);
    }
}

#[test]
fn test_learned_cost_estimate() {
    let op = LearnedFusionOperator::new(
        vec![signal(&[(1, 0.8)]), signal(&[(2, 0.7)])],
        LearnedFusion::new(2, 0.0),
    );
    assert!(op.cost_estimate(&IndexStats::new(100)) >= 0.0);
}
