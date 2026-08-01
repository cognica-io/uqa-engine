//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_operators::{DeepFusionLayer, ProgressiveFusionEntry};

fn term(field: &str) -> OperatorTree {
    OperatorTree::Term {
        query: "q".into(),
        field: Some(field.into()),
        scoring: Some(uqa_operators::TextScoringMode::BM25),
        top_k: None,
    }
}

fn membership_filter(field: &str, value: i64) -> OperatorTree {
    OperatorTree::Filter {
        field: field.into(),
        predicate: Predicate::Equals(uqa_core::Value::Int(value)),
        source: None,
    }
}

#[test]
fn empty_intersect_collapses_to_intersect_empty() {
    let op = OperatorTree::Intersect(vec![term("a"), OperatorTree::Intersect(vec![])]);
    let optimised = QueryOptimizer::new().optimize(op);
    assert!(optimised.is_empty());
}

#[test]
fn empty_composition_has_the_same_empty_semantics_as_execution() {
    let empty_composition = OperatorTree::Composed(vec![]);
    assert!(empty_composition.is_empty());

    let intersection = QueryOptimizer::new().optimize(OperatorTree::Intersect(vec![
        term("a"),
        empty_composition.clone(),
    ]));
    assert!(intersection.is_empty());

    let union =
        QueryOptimizer::new().optimize(OperatorTree::Union(vec![term("a"), empty_composition]));
    assert!(matches!(union, OperatorTree::Term { .. }));
}

#[test]
fn separately_allocated_membership_terms_are_idempotent() {
    let op = OperatorTree::Intersect(vec![
        membership_filter("year", 2026),
        membership_filter("year", 2026),
    ]);
    let optimised = QueryOptimizer::new().optimize(op);
    assert!(matches!(
        optimised,
        OperatorTree::Filter {
            ref field,
            predicate: Predicate::Equals(uqa_core::Value::Int(2026)),
            source: None,
        } if field == "year"
    ));
}

#[test]
fn membership_absorption_uses_structural_equivalence() {
    let a = || membership_filter("year", 2026);
    let b = || membership_filter("year", 2025);

    let intersection = QueryOptimizer::new().optimize(OperatorTree::Intersect(vec![
        a(),
        OperatorTree::Union(vec![b(), a()]),
    ]));
    assert!(matches!(
        intersection,
        OperatorTree::Filter {
            predicate: Predicate::Equals(uqa_core::Value::Int(2026)),
            ..
        }
    ));

    let union = QueryOptimizer::new().optimize(OperatorTree::Union(vec![
        a(),
        OperatorTree::Intersect(vec![b(), a()]),
    ]));
    assert!(matches!(
        union,
        OperatorTree::Filter {
            predicate: Predicate::Equals(uqa_core::Value::Int(2026)),
            ..
        }
    ));
}

#[test]
fn commutative_membership_subtrees_compare_independent_of_order() {
    let left = OperatorTree::Union(vec![
        membership_filter("year", 2025),
        membership_filter("year", 2026),
    ]);
    let right = OperatorTree::Union(vec![
        membership_filter("year", 2026),
        membership_filter("year", 2025),
    ]);

    let optimised = QueryOptimizer::new().optimize(OperatorTree::Intersect(vec![left, right]));
    let OperatorTree::Union(terms) = optimised else {
        panic!("expected one structurally deduplicated Union");
    };
    assert_eq!(terms.len(), 2);
}

#[test]
fn structurally_distinct_membership_terms_remain_distinct() {
    let optimised = QueryOptimizer::new().optimize(OperatorTree::Intersect(vec![
        membership_filter("year", 2025),
        membership_filter("year", 2026),
    ]));
    let OperatorTree::Intersect(terms) = optimised else {
        panic!("expected distinct Intersect");
    };
    assert_eq!(terms.len(), 2);
}

#[test]
fn scored_terms_keep_their_additive_effect() {
    let op = OperatorTree::Intersect(vec![term("a"), term("a")]);
    let optimised = QueryOptimizer::new().optimize(op);
    let OperatorTree::Intersect(terms) = optimised else {
        panic!("expected scored Intersect");
    };
    assert_eq!(terms.len(), 2);
}

#[test]
fn absorption_does_not_discard_scored_branches() {
    let op = OperatorTree::Intersect(vec![
        term("a"),
        OperatorTree::Union(vec![term("b"), term("a")]),
    ]);
    let optimised = QueryOptimizer::new().optimize(op);
    let OperatorTree::Intersect(terms) = optimised else {
        panic!("expected scored Intersect");
    };
    assert_eq!(terms.len(), 2);
    assert!(terms
        .iter()
        .any(|term| matches!(term, OperatorTree::Union(_))));
}

#[test]
fn merge_vector_thresholds_keeps_max() {
    let v1 = OperatorTree::VectorSimilarity {
        query_vector: vec![1.0, 0.0],
        threshold: 0.5,
        field: "emb".into(),
    };
    let v2 = OperatorTree::VectorSimilarity {
        query_vector: vec![1.0, 0.0],
        threshold: 0.7,
        field: "emb".into(),
    };
    let op = OperatorTree::Intersect(vec![v1, v2]);
    let optimised = QueryOptimizer::new().optimize(op);
    match optimised {
        OperatorTree::VectorSimilarity { threshold, .. } => {
            assert!((threshold - 0.7).abs() < 1e-6);
        }
        _ => panic!("expected single VectorSimilarity"),
    }
}

#[test]
fn optimizer_reaches_children_inside_physical_wrappers() {
    let vector = |threshold| OperatorTree::VectorSimilarity {
        query_vector: vec![1.0, 0.0],
        threshold,
        field: "emb".into(),
    };
    let op = OperatorTree::Opaque {
        kind: "test_wrapper".into(),
        children: vec![OperatorTree::DeepFusion {
            layers: vec![DeepFusionLayer::Signal {
                signals: vec![OperatorTree::ProgressiveFusion {
                    stages: vec![ProgressiveFusionEntry {
                        signal: OperatorTree::MessagePassing {
                            source: Box::new(OperatorTree::Intersect(vec![
                                vector(0.5),
                                vector(0.7),
                            ])),
                        },
                        k: 10,
                    }],
                    alpha: 0.5,
                    gating: uqa_operators::GatingSpec::Pass,
                }],
            }],
            alpha: 0.5,
            gating: uqa_operators::GatingSpec::Pass,
        }],
        meta: std::collections::BTreeMap::new(),
    };

    let optimized = QueryOptimizer::new().optimize(op);
    let OperatorTree::Opaque { children, .. } = optimized else {
        panic!("expected opaque wrapper");
    };
    let OperatorTree::DeepFusion { layers, .. } = &children[0] else {
        panic!("expected deep-fusion wrapper");
    };
    let DeepFusionLayer::Signal { signals } = &layers[0] else {
        panic!("expected signal layer");
    };
    let OperatorTree::ProgressiveFusion { stages, .. } = &signals[0] else {
        panic!("expected progressive-fusion wrapper");
    };
    let OperatorTree::MessagePassing { source } = &stages[0].signal else {
        panic!("expected message-passing wrapper");
    };
    let OperatorTree::VectorSimilarity { threshold, .. } = source.as_ref() else {
        panic!("expected merged vector leaf");
    };
    assert!((*threshold - 0.7).abs() < f32::EPSILON);
}

#[test]
fn query_local_index_candidate_enables_physical_scan_rewrite() {
    let predicate = Predicate::Equals(uqa_core::Value::Int(2026));
    let optimized = QueryOptimizer::new()
        .with_row_count(1_000)
        .with_index_candidates(
            [IndexScanCandidate {
                index_name: "docs_year_idx".into(),
                table_name: "docs".into(),
                field: "year".into(),
                predicate: predicate.clone(),
                scan_cost: 2.0,
            }],
            "docs",
        )
        .optimize(OperatorTree::Filter {
            field: "year".into(),
            predicate,
            source: None,
        });

    assert!(matches!(
        optimized,
        OperatorTree::IndexScan {
            ref index_name,
            ref field,
            ..
        } if index_name == "docs_year_idx" && field == "year"
    ));
}
