//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coverage for the `LogOddsFusionOperator` coverage for `test_fusion_wand`.

use std::sync::Arc;

use uqa_core::{Payload, PostingEntry, PostingList};
use uqa_fusion::LogOddsFusion;
use uqa_operators::{ExecutionContext, LogOddsFusionOperator, Operator};

struct FixedOperator(Vec<(u64, f64)>);

impl Operator for FixedOperator {
    fn execute(&self, _ctx: &ExecutionContext) -> uqa_storage::StorageBackendResult<PostingList> {
        Ok(PostingList::from_unsorted(
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
        ))
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
    assert_eq!(op.execute(&ExecutionContext::new()).unwrap().len(), 2);
}

#[test]
fn test_without_top_k_returns_all() {
    let op = LogOddsFusionOperator::new(
        vec![signal(&[(1, 0.9), (2, 0.7)]), signal(&[(1, 0.8), (2, 0.6)])],
        0.5,
    );
    assert_eq!(op.execute(&ExecutionContext::new()).unwrap().len(), 2);
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
        .unwrap()
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
    .execute(&ExecutionContext::new())
    .unwrap();
    let top = LogOddsFusionOperator::new(
        vec![
            signal(&[(1, 0.9), (2, 0.7), (3, 0.5)]),
            signal(&[(1, 0.8), (2, 0.6), (3, 0.4)]),
        ],
        0.5,
    )
    .with_top_k(2)
    .execute(&ExecutionContext::new())
    .unwrap();
    for entry in &top {
        let full_entry = full.get_entry(entry.doc_id).unwrap();
        assert!((entry.payload.score - full_entry.payload.score).abs() < 1e-6);
    }
}

#[test]
fn sparse_operator_matches_lucene_formula() {
    let result = LogOddsFusionOperator::new(
        vec![signal(&[(1, 0.8), (2, 0.6)]), signal(&[(1, 0.7)])],
        0.5,
    )
    .execute(&ExecutionContext::new())
    .unwrap();
    let fusion = LogOddsFusion::new(0.5).unwrap();
    let doc_one = result.get_entry(1).unwrap().payload.score;
    let doc_two = result.get_entry(2).unwrap().payload.score;
    assert!((doc_one - fusion.fuse_sparse(&[Some(0.8), Some(0.7)])).abs() < 1e-12);
    assert!((doc_two - fusion.fuse_sparse(&[Some(0.6), None])).abs() < 1e-12);
}

#[test]
fn one_active_signal_keeps_declared_signal_count() {
    // A signal with no matches contributes neutral evidence instead of
    // dropping out: documents still fuse at the declared n = 2, so a
    // score cannot depend on whether the other signal matched anywhere
    // (Lucene PR 16410 semantics).
    let result = LogOddsFusionOperator::new(vec![signal(&[(1, 0.8), (2, 0.2)]), signal(&[])], 0.5)
        .execute(&ExecutionContext::new())
        .unwrap();
    let fusion = LogOddsFusion::new(0.5).unwrap();
    let doc_one = result.get_entry(1).unwrap().payload.score;
    let doc_two = result.get_entry(2).unwrap().payload.score;
    assert!((doc_one - fusion.fuse_sparse(&[Some(0.8), None])).abs() < 1e-12);
    assert!((doc_two - fusion.fuse_sparse(&[Some(0.2), None])).abs() < 1e-12);

    // Layout independence: the same document scores identically whether
    // the second signal matched nothing or only unrelated documents.
    let unrelated = LogOddsFusionOperator::new(
        vec![signal(&[(1, 0.8), (2, 0.2)]), signal(&[(99, 0.6)])],
        0.5,
    )
    .execute(&ExecutionContext::new())
    .unwrap();
    assert!((unrelated.get_entry(1).unwrap().payload.score - doc_one).abs() < 1e-12);
    assert!((unrelated.get_entry(2).unwrap().payload.score - doc_two).abs() < 1e-12);
}

#[test]
fn invalid_weights_are_rejected_before_execution() {
    let error = LogOddsFusionOperator::new(vec![signal(&[]), signal(&[])], 0.5)
        .with_weights(vec![0.8, 0.8])
        .execute(&ExecutionContext::new())
        .unwrap_err();
    assert!(error.to_string().contains("weights must sum to 1"));
}
