//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relational retrieval promises rows; graph encodings and synthetic carriers do not.

use super::OperatorTree;

pub(super) fn promises_documents(operator: &OperatorTree) -> bool {
    match operator {
        OperatorTree::Empty
        | OperatorTree::Term { .. }
        | OperatorTree::Phrase { .. }
        | OperatorTree::Filter { .. }
        | OperatorTree::IndexScan { .. }
        | OperatorTree::Complement(_)
        | OperatorTree::BayesianMatchWithPrior { .. }
        | OperatorTree::VectorSimilarity { .. }
        | OperatorTree::KNN { .. }
        | OperatorTree::CalibratedVectorMatch { .. }
        | OperatorTree::MultiFieldSearch { .. }
        | OperatorTree::DeepPredict { .. } => true,
        OperatorTree::Score { source, .. }
        | OperatorTree::BayesianScore { source, .. }
        | OperatorTree::CosineProbability(source)
        | OperatorTree::ProbNot { signal: source, .. }
        | OperatorTree::SparseThreshold { source, .. }
        | OperatorTree::VectorExclusion {
            positive: source, ..
        } => promises_documents(source),
        OperatorTree::Intersect(parts) => parts.iter().any(promises_documents),
        OperatorTree::Composed(parts) => parts.last().is_some_and(promises_documents),
        OperatorTree::Union(parts)
        | OperatorTree::BayesianEvidenceFusion { signals: parts, .. }
        | OperatorTree::RobustPositiveEvidencePool { signals: parts, .. }
        | OperatorTree::ProbBoolFusion { signals: parts, .. }
        | OperatorTree::AttentionFusion { signals: parts, .. }
        | OperatorTree::LearnedFusion { signals: parts, .. } => {
            parts.iter().all(promises_documents)
        }
        OperatorTree::HybridTextVector {
            term_op, vector_op, ..
        } => promises_documents(term_op) && promises_documents(vector_op),
        OperatorTree::SemanticFilter { source, vector_op } => {
            promises_documents(source) || promises_documents(vector_op)
        }
        // Graph and aggregation carriers can introduce identities unrelated to
        // the selected relation. Opaque and staged operators supply no proof.
        _ => false,
    }
}
