//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::{collections::BTreeMap, sync::Arc};

use uqa_core::{Payload, PostingEntry, PostingList, Predicate, Value};
use uqa_operators::{
    ExecutionContext, FilterOperator, IntersectOperator, Operator, OperatorTree,
    VectorSimilarityOperator,
};
use uqa_storage::{DocumentStore, MemoryDocumentStore, MemoryVectorIndex, VectorIndex};

use super::QueryOptimizer;

fn vector(query: [f32; 2], threshold: f32) -> OperatorTree {
    OperatorTree::VectorSimilarity {
        query_vector: query.to_vec(),
        threshold,
        field: "embedding".into(),
    }
}

fn context(documents: &[[f32; 2]]) -> ExecutionContext {
    let mut index = MemoryVectorIndex::new(2);
    let mut rows = MemoryDocumentStore::new();
    for (index_in_documents, document) in documents.iter().enumerate() {
        let id = u64::try_from(index_in_documents + 1).unwrap();
        index.add(id, document.to_vec()).unwrap();
        rows.put(id, BTreeMap::from([("keep".into(), Value::Bool(true))]))
            .unwrap();
    }
    ExecutionContext::new()
        .with_vector_index("embedding", Arc::new(index))
        .with_document_store(Arc::new(rows))
}

// Bind the fixture's trees to the production operators and storage implementation.
fn bind(tree: &OperatorTree) -> Arc<dyn Operator> {
    match tree {
        OperatorTree::VectorSimilarity {
            query_vector,
            threshold,
            field,
        } => Arc::new(VectorSimilarityOperator::new(
            query_vector.clone(),
            *threshold,
            field.clone(),
        )),
        OperatorTree::Intersect(children) => {
            Arc::new(IntersectOperator::new(children.iter().map(bind).collect()))
        }
        OperatorTree::Filter {
            field,
            predicate,
            source,
        } => Arc::new(FilterOperator::new(
            field.clone(),
            predicate.clone(),
            source.as_deref().map(bind),
        )),
        _ => panic!("unexpected operator in vector intersection fixture"),
    }
}

fn execute(tree: &OperatorTree, context: &ExecutionContext) -> Result<PostingList, String> {
    bind(tree)
        .execute(context)
        .map_err(|error| error.to_string())
}

#[allow(
    deprecated,
    reason = "verify both values of the retained compatibility flag"
)]
fn assert_preserved(tree: &OperatorTree, context: &ExecutionContext) {
    let expected = execute(tree, context);
    let mut enabled = QueryOptimizer::new();
    enabled.config.enable_merge_vector_thresholds = true;
    let mut disabled = QueryOptimizer::new();
    disabled.config.enable_merge_vector_thresholds = false;
    for optimizer in [QueryOptimizer::new(), enabled, disabled] {
        let optimized = optimizer.optimize(tree.clone());
        assert_eq!(execute(&optimized, context), expected);
    }
}

#[test]
fn identical_vectors_preserve_additive_scores_and_complete_postings() {
    let context = context(&[[1.0, 0.0], [0.6, 0.8], [-1.0, 0.0]]);
    for thresholds in [[0.2, 0.8], [0.8, 0.2], [0.8, 0.8]] {
        let tree = OperatorTree::Intersect(
            thresholds
                .into_iter()
                .map(|threshold| vector([1.0, 0.0], threshold))
                .collect(),
        );
        assert_eq!(
            execute(&tree, &context).unwrap(),
            PostingList::from_sorted_unchecked(vec![PostingEntry::new(
                1,
                Payload::with_score(2.0)
            )]),
        );
        assert_preserved(&tree, &context);
    }
}

#[test]
fn nearby_vectors_preserve_threshold_support_in_both_orders() {
    let context = context(&[[1e-10, 1.0], [1.0, 0.0], [-1.0, 0.0]]);
    let operands = [vector([1.0, 0.0], 0.0), vector([1.0, 5e-10], 3e-10)];
    for children in [operands.to_vec(), operands.into_iter().rev().collect()] {
        let tree = OperatorTree::Intersect(children);
        let expected = execute(&tree, &context).unwrap();
        assert_eq!(expected.doc_ids().collect::<Vec<_>>(), [1, 2]);
        assert_preserved(&tree, &context);
    }
}

#[test]
fn nested_vector_intersections_preserve_each_score_contribution() {
    let context = context(&[[1.0, 0.0]]);
    let tree = OperatorTree::Intersect(vec![
        vector([1.0, 0.0], 0.2),
        OperatorTree::Intersect(vec![vector([1.0, 0.0], 0.5), vector([1.0, 0.0], 0.8)]),
    ]);
    assert_eq!(
        execute(&tree, &context).unwrap().entries()[0].payload.score,
        3.0
    );
    assert_preserved(&tree, &context);
}

#[test]
fn vector_intersection_costs_preserve_floating_point_score_order() {
    let context = context(&[[1.0, 0.0]]);
    let tiny = 2.0_f32.powi(-54);
    let middle = OperatorTree::Filter {
        field: "keep".into(),
        predicate: Predicate::Equals(Value::Bool(true)),
        source: Some(Box::new(vector([tiny, 1.0], -1.0))),
    };
    let tree = OperatorTree::Intersect(vec![
        vector([1.0, 0.0], -1.0),
        middle,
        vector([-1.0, 0.0], -1.0),
    ]);
    // Binary64 rounds 1 + 2^-54 to 1, so the declared left fold ends at exactly zero.
    assert_eq!(
        execute(&tree, &context).unwrap().entries()[0].payload.score,
        0.0
    );
    assert_preserved(&tree, &context);
}

#[test]
fn vector_intersection_costs_preserve_empty_short_circuit_and_validation_order() {
    let context = context(&[[1.0, 0.0]]);
    let empty = OperatorTree::Filter {
        field: "keep".into(),
        predicate: Predicate::Equals(Value::Bool(false)),
        source: Some(Box::new(vector([1.0, 0.0], -1.0))),
    };
    let invalid = vector([1.0, 0.0], f32::NAN);
    let short_circuited = OperatorTree::Intersect(vec![empty.clone(), invalid.clone()]);
    assert!(execute(&short_circuited, &context).unwrap().is_empty());
    assert_preserved(&short_circuited, &context);
    let validated = OperatorTree::Intersect(vec![invalid, empty]);
    assert!(execute(&validated, &context).is_err());
    assert_preserved(&validated, &context);
}

#[test]
fn invalid_vector_thresholds_remain_errors_in_both_orders() {
    let context = context(&[[1.0, 0.0]]);
    for threshold in [f32::NAN, f32::NEG_INFINITY, f32::INFINITY, -2.0, 2.0] {
        let operands = [vector([1.0, 0.0], threshold), vector([1.0, 0.0], 0.8)];
        for children in [operands.to_vec(), operands.into_iter().rev().collect()] {
            let tree = OperatorTree::Intersect(children);
            let error = execute(&tree, &context).unwrap_err();
            assert!(error.contains("threshold must be finite and in [-1, 1]"));
            assert_preserved(&tree, &context);
        }
    }
}

#[test]
fn threshold_endpoints_preserve_negative_and_zero_scores() {
    let context = context(&[[1.0, 0.0], [0.0, 1.0], [-1.0, 0.0]]);
    for threshold in [-1.0, 0.0, 1.0] {
        let tree = OperatorTree::Intersect(vec![
            vector([1.0, 0.0], -1.0),
            vector([1.0, 0.0], threshold),
        ]);
        assert_preserved(&tree, &context);
    }
}
