//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use uqa_core::{Payload, PostingEntry, Value};

fn residual(field: &str) -> OperatorTree {
    OperatorTree::Filter {
        field: field.into(),
        predicate: Predicate::Equals(Value::Bool(true)),
        source: None,
    }
}

fn retrieval(name: &str) -> OperatorTree {
    OperatorTree::KNN {
        field: name.into(),
        query_vector: vec![1.0, 0.0],
        k: 3,
    }
}

fn posting(ids: &[u64], score: f64, label: &str) -> PostingList {
    PostingList::from_sorted_unchecked(
        ids.iter()
            .map(|&id| {
                PostingEntry::new(
                    id,
                    Payload {
                        score,
                        positions: vec![id as u32],
                        fields: [("origin".into(), Value::Str(label.into()))].into(),
                    },
                )
            })
            .collect(),
    )
}

#[test]
fn residual_reads_follow_candidate_count_and_keep_original_payload_order() {
    for workers in [0, 4] {
        for corpus_size in [128, 16384] {
            for filter_position in 0..=2 {
                let mut parts = vec![retrieval("left"), retrieval("right")];
                parts.insert(filter_position, residual("visible"));
                let left = posting(&[2, 5, 9], 0.25, "left");
                let right = posting(&[5, 9, 11], 0.5, "right");
                let complete_filter = PostingList::from_sorted_unchecked(
                    (1..=corpus_size)
                        .filter(|id| id % 2 == 1)
                        .map(|id| PostingEntry::new(id, Payload::default()))
                        .collect(),
                );
                let expected = merge(
                    parts
                        .iter()
                        .map(|part| match part {
                            OperatorTree::Filter { .. } => complete_filter.clone().into(),
                            OperatorTree::KNN { field, .. } if field == "left" => {
                                left.clone().into()
                            }
                            _ => right.clone().into(),
                        })
                        .collect(),
                    false,
                )
                .unwrap();
                let reads = AtomicUsize::new(0);
                let retrievals = AtomicUsize::new(0);
                let result = execute(
                    &parts,
                    &ParallelExecutor::new(workers),
                    |part| {
                        retrievals.fetch_add(1, Ordering::Relaxed);
                        let OperatorTree::KNN { field, k, .. } = part else {
                            panic!("retrieval")
                        };
                        assert_eq!(*k, 3);
                        Ok(if field == "left" {
                            left.clone()
                        } else {
                            right.clone()
                        }
                        .into())
                    },
                    |_, predicate, candidates| {
                        let Some(Candidates::Documents(candidates)) = candidates else {
                            panic!("retrieval support must promise documents");
                        };
                        assert_eq!(candidates, &[5, 9]);
                        reads.fetch_add(candidates.len(), Ordering::Relaxed);
                        Ok(PostingList::from_sorted_unchecked(
                            candidates
                                .iter()
                                .filter(|id| predicate.evaluate(Some(&Value::Bool(**id % 2 == 1))))
                                .map(|&id| PostingEntry::new(id, Payload::default()))
                                .collect(),
                        ))
                    },
                )
                .unwrap();
                assert_eq!(result, expected);
                assert_eq!(reads.load(Ordering::Relaxed), 2);
                assert_eq!(retrievals.load(Ordering::Relaxed), 2);
            }
        }
    }
}

#[test]
fn empty_support_still_validates_filters_and_preserves_error_precedence() {
    let result = execute(
        &[residual("missing"), OperatorTree::Empty],
        &ParallelExecutor::new(0),
        |_| Ok(PostingList::new().into()),
        |field, _, candidates| {
            assert_eq!(candidates, Some(Candidates::Documents([].as_slice())));
            Err(SQLError::UnknownColumn(field.into()))
        },
    );
    assert!(matches!(result, Err(SQLError::UnknownColumn(field)) if field == "missing"));
    let result = execute(
        &[residual("first"), retrieval("second")],
        &ParallelExecutor::new(4),
        |_| Err(SQLError::Internal("second".into())),
        |field, _, candidates| {
            assert!(candidates.is_none());
            Err(SQLError::UnknownColumn(field.into()))
        },
    );
    assert!(matches!(result, Err(SQLError::UnknownColumn(field)) if field == "first"));
}

#[test]
fn pure_filters_and_other_carriers_keep_their_execution_contract() {
    let result = execute(
        &[residual("a"), residual("b")],
        &ParallelExecutor::new(4),
        |_| panic!("no retrieval"),
        |_, _, candidates| {
            assert!(candidates.is_none());
            Ok(posting(&[3], 9.0, "ignored"))
        },
    )
    .unwrap();
    assert_eq!(
        result.as_posting().unwrap().entries()[0].payload,
        Payload::default()
    );
    let result = execute(
        &[residual("a"), retrieval("tuple")],
        &ParallelExecutor::new(0),
        |_| {
            Ok(OperatorOutput::Generalized(
                uqa_core::GeneralizedPostingList::new(),
            ))
        },
        |_, _, candidates| {
            assert!(candidates.is_none());
            Ok(PostingList::new())
        },
    );
    assert!(matches!(result, Err(SQLError::TypeMismatch(_))));
}

#[test]
fn residual_candidates_keep_relational_promises_through_set_operations() {
    let indexed = OperatorTree::IndexScan {
        index_name: "records_id".into(),
        field: "id".into(),
        predicate: Predicate::Equals(Value::Int(1)),
    };
    let graph = OperatorTree::EncodeGraphPosting {
        source: Box::new(OperatorTree::GraphEdges {
            graph: "network".into(),
            label: None,
        }),
    };
    for (retrieval, promised) in [
        (indexed.clone(), true),
        (graph.clone(), false),
        (
            OperatorTree::Intersect(vec![indexed.clone(), graph.clone()]),
            true,
        ),
        (
            OperatorTree::Union(vec![indexed.clone(), graph.clone()]),
            false,
        ),
        (
            OperatorTree::Union(vec![indexed.clone(), indexed.clone()]),
            true,
        ),
        (
            OperatorTree::Composed(vec![indexed.clone(), graph.clone()]),
            false,
        ),
        (OperatorTree::Composed(vec![graph, indexed]), true),
    ] {
        for workers in [0, 4] {
            let result = execute(
                &[retrieval.clone(), residual("status")],
                &ParallelExecutor::new(workers),
                |_| Ok(posting(&[1, 99], 0.0, "source").into()),
                |_, _, candidates| {
                    assert_eq!(
                        candidates,
                        Some(if promised {
                            Candidates::Documents(&[1, 99])
                        } else {
                            Candidates::Intersection(&[1, 99])
                        })
                    );
                    Ok(PostingList::new())
                },
            )
            .unwrap();
            assert!(result.as_posting().unwrap().is_empty());
        }
    }
}
