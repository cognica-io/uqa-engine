//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Operator-tree field, graph, and text-signal inspection.

use super::{BTreeSet, DriverResult, OperatorTree, SQLError};

pub(super) fn require_graph_name(tree: &OperatorTree, context: &str) -> DriverResult<String> {
    let mut names = BTreeSet::new();
    collect_graph_names(tree, &mut names);
    let mut iter = names.iter();
    match (iter.next(), iter.next()) {
        (Some(name), None) => Ok(name.clone()),
        (None, _) => Err(SQLError::TypeMismatch(format!(
            "{context} does not identify a graph"
        ))),
        _ => Err(SQLError::TypeMismatch(format!(
            "{context} spans multiple graphs: {names:?}"
        ))),
    }
}

pub(super) fn require_text_field(tree: &OperatorTree, context: &str) -> DriverResult<String> {
    first_text_field(tree)
        .ok_or_else(|| SQLError::TypeMismatch(format!("{context} does not identify a text field")))
}

pub(super) fn require_vector_field(tree: &OperatorTree, context: &str) -> DriverResult<String> {
    first_vector_field(tree).ok_or_else(|| {
        SQLError::TypeMismatch(format!("{context} does not identify a vector field"))
    })
}

pub(super) fn require_shared_structured_field(
    left: &OperatorTree,
    right: &OperatorTree,
    context: &str,
) -> DriverResult<(String, String)> {
    let left = first_structured_field(left).ok_or_else(|| {
        SQLError::TypeMismatch(format!(
            "{context}.left does not identify a structured field"
        ))
    })?;
    let right = first_structured_field(right).ok_or_else(|| {
        SQLError::TypeMismatch(format!(
            "{context}.right does not identify a structured field"
        ))
    })?;
    Ok((left, right))
}

pub(super) fn require_shared_vector_field(
    left: &OperatorTree,
    right: &OperatorTree,
    context: &str,
) -> DriverResult<(String, String)> {
    Ok((
        require_vector_field(left, &format!("{context}.left"))?,
        require_vector_field(right, &format!("{context}.right"))?,
    ))
}

pub(super) fn first_text_field(tree: &OperatorTree) -> Option<String> {
    match tree {
        OperatorTree::Term { field, .. } => field.clone(),
        OperatorTree::Score { field, .. }
        | OperatorTree::BayesianScore {
            field: Some(field), ..
        }
        | OperatorTree::BayesianMatchWithPrior { field, .. } => Some(field.clone()),
        OperatorTree::MultiFieldSearch { fields, .. } if fields.len() == 1 => {
            fields.first().cloned()
        }
        OperatorTree::Intersect(children)
        | OperatorTree::Union(children)
        | OperatorTree::Composed(children) => children.iter().find_map(first_text_field),
        _ => first_child(tree).and_then(first_text_field),
    }
}

pub(super) fn first_vector_field(tree: &OperatorTree) -> Option<String> {
    match tree {
        OperatorTree::VectorSimilarity { field, .. }
        | OperatorTree::KNN { field, .. }
        | OperatorTree::CalibratedVectorMatch { field, .. } => Some(field.clone()),
        OperatorTree::GraphEmbedding { .. } => Some("_embedding".to_string()),
        OperatorTree::HybridTextVector { vector_op, .. }
        | OperatorTree::SemanticFilter { vector_op, .. }
        | OperatorTree::FacetVector { vector_op, .. } => first_vector_field(vector_op),
        OperatorTree::VectorExclusion { positive, negative } => {
            first_vector_field(positive).or_else(|| first_vector_field(negative))
        }
        OperatorTree::Intersect(children)
        | OperatorTree::Union(children)
        | OperatorTree::Composed(children) => children.iter().find_map(first_vector_field),
        _ => first_child(tree).and_then(first_vector_field),
    }
}

pub(super) fn first_structured_field(tree: &OperatorTree) -> Option<String> {
    match tree {
        OperatorTree::Filter { field, .. }
        | OperatorTree::Facet { field, .. }
        | OperatorTree::IndexScan { field, .. }
        | OperatorTree::Aggregate { field, .. }
        | OperatorTree::BayesianMatchWithPrior { field, .. }
        | OperatorTree::CalibratedVectorMatch { field, .. } => Some(field.clone()),
        OperatorTree::GroupBy { group_field, .. } => Some(group_field.clone()),
        OperatorTree::Intersect(children)
        | OperatorTree::Union(children)
        | OperatorTree::Composed(children) => children.iter().find_map(first_structured_field),
        _ => first_child(tree).and_then(first_structured_field),
    }
}

pub(super) fn first_child(tree: &OperatorTree) -> Option<&OperatorTree> {
    match tree {
        OperatorTree::Filter {
            source: Some(source),
            ..
        }
        | OperatorTree::Facet {
            source: Some(source),
            ..
        }
        | OperatorTree::Score { source, .. }
        | OperatorTree::BayesianScore { source, .. }
        | OperatorTree::Complement(source)
        | OperatorTree::EncodeGraphPosting { source }
        | OperatorTree::CosineProbability(source)
        | OperatorTree::ProbNot { signal: source, .. }
        | OperatorTree::SparseThreshold { source, .. }
        | OperatorTree::VertexAggregation { source, .. }
        | OperatorTree::MessagePassing { source }
        | OperatorTree::GraphEmbedding { source }
        | OperatorTree::GroupBy { source, .. }
        | OperatorTree::SemanticFilter { source, .. }
        | OperatorTree::Aggregate {
            source: Some(source),
            ..
        }
        | OperatorTree::GraphJoin { left: source, .. }
        | OperatorTree::TextSimilarityJoin { left: source, .. }
        | OperatorTree::VectorSimilarityJoin { left: source, .. }
        | OperatorTree::HybridJoin { left: source, .. }
        | OperatorTree::CrossParadigmJoin { left: source, .. }
        | OperatorTree::HybridTextVector {
            term_op: source, ..
        }
        | OperatorTree::VectorExclusion {
            positive: source, ..
        }
        | OperatorTree::FacetVector {
            vector_op: source, ..
        } => Some(source),
        OperatorTree::Intersect(children)
        | OperatorTree::Union(children)
        | OperatorTree::Composed(children)
        | OperatorTree::Opaque { children, .. } => children.first(),
        OperatorTree::BayesianEvidenceFusion { signals, .. }
        | OperatorTree::RobustPositiveEvidencePool { signals, .. }
        | OperatorTree::ProbBoolFusion { signals, .. }
        | OperatorTree::AttentionFusion { signals, .. }
        | OperatorTree::LearnedFusion { signals, .. } => signals.first(),
        OperatorTree::MultiStage { stages } => stages.first().map(|stage| &stage.child),
        OperatorTree::ProgressiveFusion { stages, .. } => stages.first().map(|stage| &stage.signal),
        OperatorTree::DeepFusion { layers, .. } => layers.iter().find_map(|layer| match layer {
            uqa_operators::DeepFusionLayer::Signal { signals } => signals.first(),
            _ => None,
        }),
        _ => None,
    }
}

pub(super) fn collect_graph_names(tree: &OperatorTree, names: &mut BTreeSet<String>) {
    tree.visit(&mut |node| {
        let graph = match node {
            OperatorTree::Traverse { graph, .. }
            | OperatorTree::PatternMatch { graph, .. }
            | OperatorTree::RegularPathQuery { graph, .. }
            | OperatorTree::WeightedPathQuery { graph, .. }
            | OperatorTree::GraphJoin { graph, .. }
            | OperatorTree::PageRank { graph }
            | OperatorTree::HITS { graph }
            | OperatorTree::BetweennessCentrality { graph }
            | OperatorTree::TemporalTraverse { graph, .. }
            | OperatorTree::TemporalPatternMatch { graph, .. } => Some(graph),
            _ => None,
        };
        if let Some(graph) = graph {
            names.insert(graph.clone());
        }
    });
}

/// Walk a slice of fusion signals and find the first text-bearing
/// node so attention's query-feature extractor has a query to score
/// against. Returns `(field, query)` of the first matching `Term` (or
/// `Score`-wrapped `Term`); falls back to `None` when no text signal
/// is present in the fusion args.
pub(super) fn first_text_signal(signals: &[OperatorTree]) -> Option<(String, String)> {
    for sig in signals {
        if let Some(pair) = find_text_in_tree(sig) {
            return Some(pair);
        }
    }
    None
}

pub(super) fn find_text_in_tree(tree: &OperatorTree) -> Option<(String, String)> {
    match tree {
        OperatorTree::Term { query, field, .. } => field.clone().map(|f| (f, query.clone())),
        OperatorTree::BayesianMatchWithPrior { field, query, .. } => {
            Some((field.clone(), query.clone()))
        }
        OperatorTree::Score {
            source,
            query_terms,
            field,
            ..
        } => {
            // Score wraps a Term; flatten the underlying query string
            // back out by joining the analyzed terms with spaces.
            if let Some(inner) = find_text_in_tree(source) {
                return Some(inner);
            }
            Some((field.clone(), query_terms.join(" ")))
        }
        OperatorTree::Filter {
            source: Some(s), ..
        } => find_text_in_tree(s),
        OperatorTree::Composed(parts)
        | OperatorTree::Intersect(parts)
        | OperatorTree::Union(parts) => parts.iter().find_map(find_text_in_tree),
        OperatorTree::Complement(inner)
        | OperatorTree::CosineProbability(inner)
        | OperatorTree::BayesianScore { source: inner, .. } => find_text_in_tree(inner),
        _ => None,
    }
}
