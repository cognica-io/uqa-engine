//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coverage for operator-level cases in `test_sparse_threshold`.

use std::sync::Arc;

use uqa_core::{IndexStats, Payload, PostingEntry, PostingList};
use uqa_operators::{ExecutionContext, Operator, SparseThresholdOperator};

struct FixedScoreOperator {
    entries: Vec<PostingEntry>,
}

impl FixedScoreOperator {
    fn new(entries: Vec<PostingEntry>) -> Arc<Self> {
        Arc::new(Self { entries })
    }
}

impl Operator for FixedScoreOperator {
    fn execute(&self, _ctx: &ExecutionContext) -> PostingList {
        PostingList::from_unsorted(self.entries.clone())
    }
}

fn entry(doc_id: u64, score: f64) -> PostingEntry {
    PostingEntry::new(doc_id, Payload::with_score(score))
}

fn source(entries: Vec<PostingEntry>) -> Arc<dyn Operator> {
    FixedScoreOperator::new(entries)
}

#[test]
fn filters_below_threshold() {
    let op = SparseThresholdOperator::new(
        source(vec![entry(1, 0.3), entry(2, 0.7), entry(3, 0.5)]),
        0.5,
    );
    let result = op.execute(&ExecutionContext::new());
    assert_eq!(result.len(), 1);
    assert_eq!(result.entries()[0].doc_id, 2);
    assert!((result.entries()[0].payload.score - 0.2).abs() < 1e-9);
}

#[test]
fn zero_threshold_keeps_all_positive() {
    let op = SparseThresholdOperator::new(source(vec![entry(1, 0.1), entry(2, 0.5)]), 0.0);
    assert_eq!(op.execute(&ExecutionContext::new()).len(), 2);
}

#[test]
fn high_threshold_excludes_all() {
    let op = SparseThresholdOperator::new(source(vec![entry(1, 0.3), entry(2, 0.5)]), 1.0);
    assert_eq!(op.execute(&ExecutionContext::new()).len(), 0);
}

#[test]
fn exact_threshold_excluded() {
    let op = SparseThresholdOperator::new(source(vec![entry(1, 0.5)]), 0.5);
    assert_eq!(op.execute(&ExecutionContext::new()).len(), 0);
}

#[test]
fn adjusted_scores() {
    let op = SparseThresholdOperator::new(source(vec![entry(1, 0.8), entry(2, 0.6)]), 0.3);
    let result = op.execute(&ExecutionContext::new());
    assert_eq!(result.len(), 2);
    let score_1 = result.get_entry(1).unwrap().payload.score;
    let score_2 = result.get_entry(2).unwrap().payload.score;
    assert!((score_1 - 0.5).abs() < 1e-9);
    assert!((score_2 - 0.3).abs() < 1e-9);
}

#[test]
fn preserves_doc_id_order() {
    let op = SparseThresholdOperator::new(
        source(vec![entry(1, 0.9), entry(5, 0.8), entry(10, 0.7)]),
        0.1,
    );
    let ids: Vec<_> = op.execute(&ExecutionContext::new()).doc_ids().collect();
    assert_eq!(ids, vec![1, 5, 10]);
}

#[test]
fn cost_estimate() {
    let src = source(vec![entry(1, 0.5)]);
    let op = SparseThresholdOperator::new(src.clone(), 0.3);
    let stats = IndexStats::new(100);
    assert_eq!(op.cost_estimate(&stats), src.cost_estimate(&stats));
}

#[test]
fn empty_source() {
    let op = SparseThresholdOperator::new(source(vec![]), 0.5);
    assert_eq!(op.execute(&ExecutionContext::new()).len(), 0);
}
