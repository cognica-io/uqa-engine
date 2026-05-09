//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Port of the `LogOddsFusionOperator` portion of Python `test_fusion_wand.py`.

use std::sync::Arc;

use uqa_core::{Payload, PostingEntry, PostingList};
use uqa_operators::{ExecutionContext, LogOddsFusionOperator, Operator};

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
fn test_top_k_parameter() {
    let op = LogOddsFusionOperator::new(
        vec![
            signal(&[(1, 0.9), (2, 0.7), (3, 0.5), (4, 0.3)]),
            signal(&[(1, 0.8), (2, 0.6), (3, 0.4), (4, 0.2)]),
        ],
        0.5,
    )
    .with_top_k(2);
    assert_eq!(op.execute(&ExecutionContext::new()).len(), 2);
}

#[test]
fn test_without_top_k_returns_all() {
    let op = LogOddsFusionOperator::new(
        vec![signal(&[(1, 0.9), (2, 0.7)]), signal(&[(1, 0.8), (2, 0.6)])],
        0.5,
    );
    assert_eq!(op.execute(&ExecutionContext::new()).len(), 2);
}

#[test]
fn test_top_k_preserves_ranking() {
    let op = LogOddsFusionOperator::new(
        vec![
            signal(&[(1, 0.9), (2, 0.3), (3, 0.1)]),
            signal(&[(1, 0.8), (2, 0.2), (3, 0.1)]),
        ],
        0.5,
    )
    .with_top_k(2);
    let ids: Vec<u64> = op
        .execute(&ExecutionContext::new())
        .iter()
        .map(|entry| entry.doc_id)
        .collect();
    assert!(ids.contains(&1));
}

#[test]
fn test_top_k_results_match_full_results() {
    let full = LogOddsFusionOperator::new(
        vec![
            signal(&[(1, 0.9), (2, 0.7), (3, 0.5)]),
            signal(&[(1, 0.8), (2, 0.6), (3, 0.4)]),
        ],
        0.5,
    )
    .execute(&ExecutionContext::new());
    let top = LogOddsFusionOperator::new(
        vec![
            signal(&[(1, 0.9), (2, 0.7), (3, 0.5)]),
            signal(&[(1, 0.8), (2, 0.6), (3, 0.4)]),
        ],
        0.5,
    )
    .with_top_k(2)
    .execute(&ExecutionContext::new());
    for entry in &top {
        let full_entry = full.get_entry(entry.doc_id).unwrap();
        assert!((entry.payload.score - full_entry.payload.score).abs() < 1e-6);
    }
}
