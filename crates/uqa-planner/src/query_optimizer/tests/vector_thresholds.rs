//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;

use uqa_core::{Payload, PostingEntry, PostingList};
use uqa_operators::{
    ExecutionContext, IntersectOperator, Operator, OperatorTree, VectorSimilarityOperator,
};
use uqa_storage::{MemoryVectorIndex, VectorIndex};

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
    for (index_in_documents, document) in documents.iter().enumerate() {
        index
            .add(
                u64::try_from(index_in_documents + 1).unwrap(),
                document.to_vec(),
            )
            .unwrap();
    }
    ExecutionContext::new().with_vector_index("embedding", Arc::new(index))
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
