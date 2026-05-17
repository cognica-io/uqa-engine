//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coverage for `test_progressive_fusion`.

use std::sync::Arc;

use uqa_core::{IndexStats, Payload, PostingEntry, PostingList};
use uqa_operators::{ExecutionContext, Operator, ProgressiveFusionOperator};

struct FixedOperator {
    entries: Vec<(u64, f64)>,
}

impl FixedOperator {
    fn new(entries: Vec<(u64, f64)>) -> Arc<Self> {
        Arc::new(Self { entries })
    }
}

impl Operator for FixedOperator {
    fn execute(&self, _ctx: &ExecutionContext) -> PostingList {
        PostingList::from_unsorted(
            self.entries
                .iter()
                .map(|(doc_id, score)| PostingEntry::new(*doc_id, Payload::with_score(*score)))
                .collect(),
        )
    }
}

fn op(entries: Vec<(u64, f64)>) -> Arc<dyn Operator> {
    FixedOperator::new(entries)
}

#[test]
fn two_stage_basic() {
    let sig1 = op(vec![(1, 0.9), (2, 0.7), (3, 0.5), (4, 0.3)]);
    let sig2 = op(vec![(1, 0.8), (2, 0.6), (3, 0.4), (4, 0.2)]);
    let sig3 = op(vec![(1, 0.7), (2, 0.5)]);

    let op = ProgressiveFusionOperator::new(vec![(vec![sig1, sig2], 3), (vec![sig3], 2)], 0.5);
    let result = op.execute(&ExecutionContext::new());
    let doc_ids: Vec<_> = result.doc_ids().collect();
    assert_eq!(result.len(), 2);
    assert!(doc_ids.contains(&1));
}

#[test]
fn single_stage_equivalence() {
    let sig1 = op(vec![(1, 0.9), (2, 0.3)]);
    let sig2 = op(vec![(1, 0.8), (2, 0.2)]);

    let op = ProgressiveFusionOperator::new(vec![(vec![sig1, sig2], 1)], 0.5);
    let result = op.execute(&ExecutionContext::new());
    assert_eq!(result.len(), 1);
    assert_eq!(result.entries()[0].doc_id, 1);
}

#[test]
fn three_stage_narrowing() {
    let sig1 = op((1..11)
        .map(|i| (i, 0.9 - f64::from(i as u32) * 0.05))
        .collect());
    let sig2 = op((1..11)
        .map(|i| (i, 0.8 - f64::from(i as u32) * 0.04))
        .collect());
    let sig3 = op((1..11)
        .map(|i| (i, 0.7 - f64::from(i as u32) * 0.03))
        .collect());

    let op = ProgressiveFusionOperator::new(
        vec![(vec![sig1], 8), (vec![sig2], 5), (vec![sig3], 3)],
        0.5,
    );
    let result = op.execute(&ExecutionContext::new());
    assert_eq!(result.len(), 3);
}

#[test]
fn cost_cascading() {
    let sig1 = op(vec![(1, 0.9)]);
    let sig2 = op(vec![(1, 0.8)]);

    let op = ProgressiveFusionOperator::new(vec![(vec![sig1], 50), (vec![sig2], 10)], 0.5);
    let cost = op.cost_estimate(&IndexStats::new(100));
    assert!(cost > 0.0);
}

#[test]
fn gating_forwarded() {
    let sig1 = op(vec![(1, 0.9)]);
    let sig2 = op(vec![(1, 0.8)]);

    let op = ProgressiveFusionOperator::with_gating(
        vec![(vec![sig1, sig2], 1)],
        0.5,
        Some("relu".into()),
    );
    assert_eq!(op.gating.as_deref(), Some("relu"));
    let result = op.execute(&ExecutionContext::new());
    assert_eq!(result.len(), 1);
}

#[test]
#[should_panic(expected = "at least one stage")]
fn empty_stages_raises() {
    let _ = ProgressiveFusionOperator::new(Vec::new(), 0.5);
}

#[test]
fn scores_are_probabilities() {
    let sig1 = op(vec![(1, 0.7), (2, 0.6), (3, 0.5)]);
    let sig2 = op(vec![(1, 0.8), (2, 0.5), (3, 0.3)]);

    let op = ProgressiveFusionOperator::new(vec![(vec![sig1, sig2], 3)], 0.5);
    let result = op.execute(&ExecutionContext::new());
    for entry in &result {
        assert!(entry.payload.score > 0.0);
        assert!(entry.payload.score < 1.0);
    }
}
