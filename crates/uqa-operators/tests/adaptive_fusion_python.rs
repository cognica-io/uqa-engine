//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ports operator-level cases from `uqa/tests/test_adaptive_fusion.py`.

use std::sync::Arc;

use uqa_core::{IndexStats, Payload, PostingEntry, PostingList};
use uqa_operators::{AdaptiveLogOddsFusionOperator, ExecutionContext, Operator};

struct ConstantOperator {
    pl: PostingList,
}

impl ConstantOperator {
    fn new(entries: Vec<(u64, f64)>) -> Arc<Self> {
        Arc::new(Self {
            pl: PostingList::from_unsorted(
                entries
                    .into_iter()
                    .map(|(doc_id, score)| PostingEntry::new(doc_id, Payload::with_score(score)))
                    .collect(),
            ),
        })
    }
}

impl Operator for ConstantOperator {
    fn execute(&self, _ctx: &ExecutionContext) -> PostingList {
        self.pl.clone()
    }
}

fn op(entries: Vec<(u64, f64)>) -> Arc<dyn Operator> {
    ConstantOperator::new(entries)
}

#[test]
fn adaptive_operator_basic() {
    let pl1 = op(vec![(1, 0.8), (2, 0.7), (3, 0.6)]);
    let pl2 = op(vec![(1, 0.9), (2, 0.3)]);

    let op = AdaptiveLogOddsFusionOperator::new(vec![pl1, pl2], 0.5, None);
    let result = op.execute(&ExecutionContext::new());

    assert_eq!(result.len(), 3);
    assert_eq!(result.doc_ids().collect::<Vec<_>>(), vec![1, 2, 3]);
    for entry in &result {
        assert!(entry.payload.score > 0.0);
        assert!(entry.payload.score < 1.0);
    }
}

#[test]
fn adaptive_operator_empty() {
    let op = AdaptiveLogOddsFusionOperator::new(Vec::new(), 0.5, None);
    let result = op.execute(&ExecutionContext::new());
    assert_eq!(result.len(), 0);
}

#[test]
fn adaptive_operator_cost_estimate() {
    let pl1 = op(vec![(1, 0.5)]);
    let pl2 = op(vec![(2, 0.5)]);

    let op = AdaptiveLogOddsFusionOperator::new(vec![pl1, pl2], 0.5, None);
    assert_eq!(op.cost_estimate(&IndexStats::new(100)), 200.0);
}
