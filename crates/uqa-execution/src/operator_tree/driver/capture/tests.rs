//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::cell::RefCell;
use uqa_core::{Payload, PostingEntry, PostingList, ScoredEntry};
use uqa_sql::SQLError;

fn term(query: &str) -> OperatorTree {
    OperatorTree::Term {
        field: Some("body".into()),
        query: query.into(),
        scoring: Some(TextScoringMode::BM25),
        top_k: None,
    }
}

fn text_input(tree: &OperatorTree) -> (Option<&str>, &str, Option<TextScoringMode>) {
    let OperatorTree::Term {
        field,
        query,
        scoring,
        ..
    } = tree
    else {
        panic!("expected text leaf");
    };
    (field.as_deref(), query, *scoring)
}

fn entries(values: &[(u64, f64)], bytes: usize) -> DriverResult<CapturedScoredEntries> {
    let mut capture = ScoredEntryCapture::new(bytes, CancellationToken::new())?;
    for &(doc_id, score) in values {
        capture.push(ScoredEntry { doc_id, score })?;
    }
    capture.finish()
}

fn filter(field: &str) -> OperatorTree {
    OperatorTree::Filter {
        field: field.into(),
        predicate: uqa_core::Predicate::IsNotNull,
        source: None,
    }
}

#[test]
fn captured_intersection_filters_share_complete_support_and_original_score_order() {
    let tree = OperatorTree::Intersect(vec![
        filter("first"),
        term("one"),
        term("two"),
        filter("last"),
    ]);
    assert!(tree::complete_scored_tree(&tree));
    let seen = RefCell::new(Vec::new());
    let actual = tree::capture(
        &tree,
        0,
        &CancellationToken::new(),
        &|leaf, support, bytes| match leaf {
            OperatorTree::Term { query, .. } => {
                assert!(support.is_none());
                seen.borrow_mut().push(query.clone());
                entries(
                    if query == "one" {
                        &[(1, -0.0), (2, 1e16)]
                    } else {
                        &[(2, 1.0), (3, 9.0)]
                    },
                    bytes,
                )
            }
            OperatorTree::Filter { field, .. } => {
                seen.borrow_mut().push(field.clone());
                let ids = support.unwrap().cursor().collect::<Result<Vec<_>, _>>()?;
                assert_eq!(ids, [2]);
                entries(&[(2, 0.0)], bytes)
            }
            _ => panic!("unexpected leaf"),
        },
    )
    .unwrap()
    .map(Result::unwrap)
    .collect::<Vec<_>>();
    assert_eq!(*seen.borrow(), ["one", "two", "first", "last"]);
    assert_eq!(actual.len(), 1);
    assert_eq!(actual[0].doc_id, 2);
    assert_eq!(
        actual[0].score.to_bits(),
        (((0.0_f64 + 1e16) + 1.0) + 0.0).to_bits()
    );
}

#[test]
fn captured_filter_errors_keep_original_precedence_after_failed_or_empty_siblings() {
    for fail_text in [false, true] {
        let seen = RefCell::new(Vec::new());
        let tree = OperatorTree::Intersect(vec![filter("first"), term("text"), filter("last")]);
        let error = tree::capture(
            &tree,
            0,
            &CancellationToken::new(),
            &|leaf, support, bytes| match leaf {
                OperatorTree::Term { .. } => {
                    seen.borrow_mut().push("text".to_owned());
                    if fail_text {
                        Err(SQLError::UnknownColumn("text-error".into()))
                    } else {
                        entries(&[], bytes)
                    }
                }
                OperatorTree::Filter { field, .. } => {
                    seen.borrow_mut().push(field.clone());
                    assert_eq!(support.is_none(), fail_text);
                    if let Some(support) = support {
                        assert!(support.is_empty());
                    }
                    Err(SQLError::UnknownColumn(field.clone()))
                }
                _ => panic!("unexpected leaf"),
            },
        )
        .err()
        .unwrap();
        assert!(matches!(error, SQLError::UnknownColumn(field) if field == "first"));
        assert_eq!(*seen.borrow(), ["text", "first", "last"]);
    }
}

#[test]
fn captured_text_boolean_fold_preserves_support_and_native_score_bits() {
    let values = [
        vec![(0, -0.0), (1, 1e16), (3, 0.5), (u64::MAX, 1.5)],
        vec![(0, 0.0), (1, -1e16), (2, 0.25), (u64::MAX, 2.0)],
        vec![(1, 1.0), (2, 0.75), (u64::MAX, 3.0)],
    ];
    let postings = values
        .iter()
        .map(|values| {
            PostingList::from_sorted_unchecked(
                values
                    .iter()
                    .map(|&(id, score)| PostingEntry::new(id, Payload::with_score(score)))
                    .collect(),
            )
        })
        .collect::<Vec<_>>();
    for intersection in [false, true] {
        let children = vec![term("0"), term("1"), term("2")];
        let tree = if intersection {
            OperatorTree::Intersect(children)
        } else {
            OperatorTree::Union(children)
        };
        assert!(tree::complete_scored_tree(&tree));
        let actual = tree::capture(&tree, 0, &CancellationToken::new(), &|tree, _, bytes| {
            entries(&values[text_input(tree).1.parse::<usize>().unwrap()], bytes)
        })
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (entry.doc_id, entry.score.to_bits())
        })
        .collect::<Vec<_>>();
        let expected = postings[1..]
            .iter()
            .fold(postings[0].clone(), |left, right| {
                if intersection {
                    left.merge_intersection(right)
                } else {
                    left.merge_union(right)
                }
            });
        assert_eq!(
            actual,
            expected
                .entries()
                .iter()
                .map(|entry| (entry.doc_id, entry.payload.score.to_bits()))
                .collect::<Vec<_>>()
        );
        assert!(actual.contains(&(1, 1.0_f64.to_bits())));
    }
}

#[test]
fn captured_text_branches_all_run_before_the_first_error_is_reported() {
    let visited = RefCell::new(Vec::new());
    let tree = OperatorTree::Intersect(vec![
        OperatorTree::Empty,
        term("first-error"),
        term("last-error"),
    ]);
    let result = tree::capture(&tree, 1024, &CancellationToken::new(), &|tree, _, _| {
        let query = text_input(tree).1;
        visited.borrow_mut().push(query.to_owned());
        Err(uqa_sql::SQLError::Routine {
            sqlstate: "22023".into(),
            message: query.into(),
        })
    });
    let error = result.err().unwrap();
    assert_eq!(error.sqlstate(), Some("22023"));
    assert_eq!(error.to_string(), "first-error");
    assert_eq!(*visited.borrow(), ["first-error", "last-error"]);
    let empty = tree::capture(
        &OperatorTree::Union(Vec::new()),
        0,
        &CancellationToken::new(),
        &|_, _, _| panic!("empty tree has no leaves"),
    )
    .unwrap();
    assert_eq!(empty.count(), 0);
}

#[test]
fn complete_text_capture_declines_non_score_and_limited_operands() {
    assert!(tree::complete_scored_tree(&OperatorTree::Intersect(vec![
        term("one"),
        OperatorTree::Union(vec![term("two"), OperatorTree::Empty]),
    ])));
    let limited = OperatorTree::Term {
        field: Some("body".into()),
        query: "one".into(),
        scoring: Some(TextScoringMode::BM25),
        top_k: Some(uqa_operators::TextTopKPlan {
            k: 2,
            strategy: uqa_operators::TextTopKStrategy::Wand,
        }),
    };
    assert!(!tree::complete_scored_tree(&limited));
    assert!(!tree::complete_scored_tree(&OperatorTree::Union(vec![
        term("one"),
        limited,
    ])));
    assert!(!tree::complete_scored_tree(&OperatorTree::Phrase {
        field: Some("body".into()),
        query: "one two".into(),
        scoring: Some(TextScoringMode::BM25),
    }));
}

#[test]
fn complete_exact_fusion_capture_keeps_typed_signal_boundaries() {
    let exact = |signals| OperatorTree::BayesianEvidenceFusion {
        signals,
        base_rate: Some(0.1),
    };
    assert!(tree::complete_scored_tree(&exact(vec![
        term("one"),
        exact(vec![term("two"), OperatorTree::Empty]),
    ])));
    assert!(!tree::complete_scored_tree(&exact(vec![
        term("one"),
        OperatorTree::CosineProbability(Box::new(term("two"))),
    ])));
    assert!(!tree::complete_scored_tree(&exact(vec![
        OperatorTree::BayesianScore {
            source: Box::new(term("one")),
            field: Some("body".into()),
        },
    ])));
}

#[test]
fn complete_text_capture_keeps_the_all_field_leaf_boundary() {
    let tree = OperatorTree::Term {
        field: None,
        query: "alpha beta".into(),
        scoring: Some(TextScoringMode::BM25),
        top_k: None,
    };
    assert!(tree::complete_scored_tree(&tree));
    let observed = RefCell::new(0);
    let mut result = tree::capture(
        &tree,
        64 * 1024,
        &CancellationToken::new(),
        &|tree, _, bytes| {
            let (field, query, scoring) = text_input(tree);
            assert_eq!(field, None);
            assert_eq!(query, "alpha beta");
            assert_eq!(scoring, Some(TextScoringMode::BM25));
            *observed.borrow_mut() += 1;
            entries(&[(1, 0.5)], bytes)
        },
    )
    .unwrap();
    assert_eq!(*observed.borrow(), 1);
    assert_eq!(
        result.next().unwrap().unwrap().score.to_bits(),
        0.5_f64.to_bits()
    );
    assert!(result.next().is_none());
}

#[test]
fn captured_text_boolean_population_stays_within_its_shared_allowance() {
    let tree = OperatorTree::Union(vec![
        term("0"),
        OperatorTree::Intersect(vec![term("1"), term("2")]),
    ]);
    let allocated = allocation_counter::measure(|| {
        let mut result = tree::capture(
            &tree,
            64 * 1024,
            &CancellationToken::new(),
            &|tree, _, bytes| {
                let query = text_input(tree).1;
                let mut capture = ScoredEntryCapture::new(bytes, CancellationToken::new())?;
                for doc_id in 0..65_536 {
                    capture.push(ScoredEntry {
                        doc_id,
                        score: query.parse().unwrap(),
                    })?;
                }
                capture.finish()
            },
        )
        .unwrap();
        for id in 0..65_536 {
            let entry = result.next().unwrap().unwrap();
            assert_eq!(entry.doc_id, id);
            assert_eq!(entry.score.to_bits(), 3.0_f64.to_bits());
        }
        assert!(result.next().is_none());
    });
    // Three complete score vectors alone require 3MiB, excluding posting payloads.
    assert!(allocated.bytes_max < 512 * 1024, "{allocated:?}");
}
