//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical access domain classification with specific operator-join precedence.

use crate::AccessParadigm;
use uqa_operators::OperatorTree;

pub fn operator_tree_paradigm(tree: &OperatorTree) -> AccessParadigm {
    let mut text = false;
    let mut vector = false;
    let mut graph = false;
    let mut relational = false;
    let mut text_join = false;
    let mut vector_join = false;
    let mut graph_join = false;
    let mut hybrid_join = false;
    let mut cross_paradigm_join = false;
    tree.visit(&mut |node| match node {
        OperatorTree::Term { .. }
        | OperatorTree::BayesianScore { .. }
        | OperatorTree::BayesianMatchWithPrior { .. }
        | OperatorTree::MultiFieldSearch { .. } => text = true,
        OperatorTree::VectorSimilarity { .. }
        | OperatorTree::KNN { .. }
        | OperatorTree::CalibratedVectorMatch { .. }
        | OperatorTree::CosineProbability(_) => vector = true,
        OperatorTree::Traverse { .. }
        | OperatorTree::GraphNeighbors { .. }
        | OperatorTree::GraphEdges { .. }
        | OperatorTree::PatternMatch { .. }
        | OperatorTree::RegularPathQuery { .. }
        | OperatorTree::WeightedPathQuery { .. }
        | OperatorTree::PageRank { .. }
        | OperatorTree::HITS { .. }
        | OperatorTree::BetweennessCentrality { .. }
        | OperatorTree::TemporalTraverse { .. }
        | OperatorTree::TemporalPatternMatch { .. } => graph = true,
        OperatorTree::Filter { .. } | OperatorTree::IndexScan { .. } => relational = true,
        OperatorTree::TextSimilarityJoin { .. } => text_join = true,
        OperatorTree::VectorSimilarityJoin { .. } => vector_join = true,
        OperatorTree::GraphJoin { .. } => graph_join = true,
        OperatorTree::HybridJoin { .. } => hybrid_join = true,
        OperatorTree::CrossParadigmJoin { .. } => cross_paradigm_join = true,
        _ => {}
    });
    if cross_paradigm_join || graph && (text || vector || relational) {
        AccessParadigm::CrossParadigm
    } else if hybrid_join || text && vector || relational && (text || vector) {
        AccessParadigm::Hybrid
    } else if graph_join || graph {
        AccessParadigm::Graph
    } else if vector_join || vector {
        AccessParadigm::Vector
    } else if text_join || text {
        AccessParadigm::Text
    } else {
        AccessParadigm::Relational
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::Predicate;
    #[test]
    fn operator_join_paradigms_remain_specific() {
        let term = || OperatorTree::Term {
            query: "rust".into(),
            field: Some("body".into()),
            scoring: None,
            top_k: None,
        };
        let vector = || OperatorTree::KNN {
            query_vector: vec![1.0, 0.0],
            k: 3,
            field: "embedding".into(),
        };
        let filter = || OperatorTree::Filter {
            field: "category".into(),
            predicate: Predicate::IsNotNull,
            source: None,
        };
        assert_eq!(
            operator_tree_paradigm(&OperatorTree::TextSimilarityJoin {
                left: Box::new(term()),
                right: Box::new(term()),
                threshold: 0.5,
            }),
            AccessParadigm::Text
        );
        assert_eq!(
            operator_tree_paradigm(&OperatorTree::VectorSimilarityJoin {
                left: Box::new(vector()),
                right: Box::new(vector()),
                threshold: 0.5,
            }),
            AccessParadigm::Vector
        );
        assert_eq!(
            operator_tree_paradigm(&OperatorTree::GraphJoin {
                left: Box::new(OperatorTree::PageRank { graph: "g".into() }),
                right: Box::new(OperatorTree::PageRank { graph: "g".into() }),
                label: None,
                graph: "g".into(),
            }),
            AccessParadigm::Graph
        );
        assert_eq!(
            operator_tree_paradigm(&OperatorTree::HybridJoin {
                left: Box::new(OperatorTree::Intersect(vec![filter(), vector()])),
                right: Box::new(OperatorTree::Intersect(vec![filter(), vector()])),
            }),
            AccessParadigm::Hybrid
        );
        assert_eq!(
            operator_tree_paradigm(&OperatorTree::CrossParadigmJoin {
                left: Box::new(OperatorTree::PageRank { graph: "g".into() }),
                right: Box::new(filter()),
            }),
            AccessParadigm::CrossParadigm
        );
    }
}
