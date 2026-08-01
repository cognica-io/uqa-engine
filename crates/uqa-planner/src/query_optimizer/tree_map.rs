//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exhaustive child traversal for every executable operator variant.

use uqa_operators::{DeepFusionLayer, MultiStageEntry, OperatorTree, ProgressiveFusionEntry};

use super::QueryOptimizer;

impl QueryOptimizer {
    // ---------------------------------------------------------------
    // Generic recursion (used by simplify / merge_vector / reorder)
    // ---------------------------------------------------------------

    pub(super) fn recurse_children(&self, op: OperatorTree) -> OperatorTree {
        map_operator_children(op, |child| self.optimize(child))
    }
}

/// Apply one rewrite function to every direct child carried by the IR.
/// Keeping this structural traversal exhaustive prevents a newly
/// executable wrapper (join, graph embedding, progressive/deep fusion,
/// and so on) from becoming an optimizer boundary by accident.
#[allow(clippy::too_many_lines)]
pub(super) fn map_operator_children(
    op: OperatorTree,
    mut map: impl FnMut(OperatorTree) -> OperatorTree,
) -> OperatorTree {
    match op {
        OperatorTree::Filter {
            field,
            predicate,
            source,
        } => OperatorTree::Filter {
            field,
            predicate,
            source: source.map(|child| Box::new(map(*child))),
        },
        OperatorTree::Facet { field, source } => OperatorTree::Facet {
            field,
            source: source.map(|child| Box::new(map(*child))),
        },
        OperatorTree::Score {
            scorer,
            source,
            query_terms,
            field,
        } => OperatorTree::Score {
            scorer,
            source: Box::new(map(*source)),
            query_terms,
            field,
        },
        OperatorTree::BayesianScore { source, field } => OperatorTree::BayesianScore {
            source: Box::new(map(*source)),
            field,
        },
        OperatorTree::Intersect(children) => {
            OperatorTree::Intersect(children.into_iter().map(&mut map).collect())
        }
        OperatorTree::Union(children) => {
            OperatorTree::Union(children.into_iter().map(&mut map).collect())
        }
        OperatorTree::Complement(child) => OperatorTree::Complement(Box::new(map(*child))),
        OperatorTree::Composed(children) => {
            OperatorTree::Composed(children.into_iter().map(&mut map).collect())
        }
        OperatorTree::EncodeGraphPosting { source } => OperatorTree::EncodeGraphPosting {
            source: Box::new(map(*source)),
        },
        OperatorTree::CosineProbability(child) => {
            OperatorTree::CosineProbability(Box::new(map(*child)))
        }
        OperatorTree::BayesianEvidenceFusion { signals, base_rate } => {
            OperatorTree::BayesianEvidenceFusion {
                signals: signals.into_iter().map(&mut map).collect(),
                base_rate,
            }
        }
        OperatorTree::RobustPositiveEvidencePool {
            signals,
            alpha,
            gating,
            weights,
            logit_min,
            logit_max,
            adaptive_weights,
        } => OperatorTree::RobustPositiveEvidencePool {
            signals: signals.into_iter().map(&mut map).collect(),
            alpha,
            gating,
            weights,
            logit_min,
            logit_max,
            adaptive_weights,
        },
        OperatorTree::ProbBoolFusion { signals, mode } => OperatorTree::ProbBoolFusion {
            signals: signals.into_iter().map(&mut map).collect(),
            mode,
        },
        OperatorTree::ProbNot {
            signal,
            default_prob,
        } => OperatorTree::ProbNot {
            signal: Box::new(map(*signal)),
            default_prob,
        },
        OperatorTree::AttentionFusion {
            signals,
            attention,
            query_features,
        } => OperatorTree::AttentionFusion {
            signals: signals.into_iter().map(&mut map).collect(),
            attention,
            query_features,
        },
        OperatorTree::LearnedFusion { signals, learned } => OperatorTree::LearnedFusion {
            signals: signals.into_iter().map(&mut map).collect(),
            learned,
        },
        OperatorTree::SparseThreshold { source, threshold } => OperatorTree::SparseThreshold {
            source: Box::new(map(*source)),
            threshold,
        },
        OperatorTree::GraphJoin {
            left,
            right,
            label,
            graph,
        } => OperatorTree::GraphJoin {
            left: Box::new(map(*left)),
            right: Box::new(map(*right)),
            label,
            graph,
        },
        OperatorTree::Aggregate {
            source,
            field,
            monoid,
        } => OperatorTree::Aggregate {
            source: source.map(|child| Box::new(map(*child))),
            field,
            monoid,
        },
        OperatorTree::GroupBy {
            source,
            group_field,
            agg_field,
            monoid,
        } => OperatorTree::GroupBy {
            source: Box::new(map(*source)),
            group_field,
            agg_field,
            monoid,
        },
        OperatorTree::MultiStage { stages } => OperatorTree::MultiStage {
            stages: stages
                .into_iter()
                .map(|stage| MultiStageEntry {
                    child: map(stage.child),
                    cutoff: stage.cutoff,
                })
                .collect(),
        },
        OperatorTree::HybridTextVector {
            term_op,
            vector_op,
            alpha,
        } => OperatorTree::HybridTextVector {
            term_op: Box::new(map(*term_op)),
            vector_op: Box::new(map(*vector_op)),
            alpha,
        },
        OperatorTree::SemanticFilter { source, vector_op } => OperatorTree::SemanticFilter {
            source: Box::new(map(*source)),
            vector_op: Box::new(map(*vector_op)),
        },
        OperatorTree::VectorExclusion { positive, negative } => OperatorTree::VectorExclusion {
            positive: Box::new(map(*positive)),
            negative: Box::new(map(*negative)),
        },
        OperatorTree::FacetVector {
            vector_op,
            facet_field,
        } => OperatorTree::FacetVector {
            vector_op: Box::new(map(*vector_op)),
            facet_field,
        },
        OperatorTree::VertexAggregation { source, monoid } => OperatorTree::VertexAggregation {
            source: Box::new(map(*source)),
            monoid,
        },
        OperatorTree::MessagePassing { source } => OperatorTree::MessagePassing {
            source: Box::new(map(*source)),
        },
        OperatorTree::GraphEmbedding { source } => OperatorTree::GraphEmbedding {
            source: Box::new(map(*source)),
        },
        OperatorTree::TextSimilarityJoin {
            left,
            right,
            threshold,
        } => OperatorTree::TextSimilarityJoin {
            left: Box::new(map(*left)),
            right: Box::new(map(*right)),
            threshold,
        },
        OperatorTree::VectorSimilarityJoin {
            left,
            right,
            threshold,
        } => OperatorTree::VectorSimilarityJoin {
            left: Box::new(map(*left)),
            right: Box::new(map(*right)),
            threshold,
        },
        OperatorTree::HybridJoin { left, right } => OperatorTree::HybridJoin {
            left: Box::new(map(*left)),
            right: Box::new(map(*right)),
        },
        OperatorTree::CrossParadigmJoin { left, right } => OperatorTree::CrossParadigmJoin {
            left: Box::new(map(*left)),
            right: Box::new(map(*right)),
        },
        OperatorTree::ProgressiveFusion {
            stages,
            alpha,
            gating,
        } => OperatorTree::ProgressiveFusion {
            stages: stages
                .into_iter()
                .map(|stage| ProgressiveFusionEntry {
                    signal: map(stage.signal),
                    k: stage.k,
                })
                .collect(),
            alpha,
            gating,
        },
        OperatorTree::DeepFusion {
            layers,
            alpha,
            gating,
        } => OperatorTree::DeepFusion {
            layers: layers
                .into_iter()
                .map(|layer| match layer {
                    DeepFusionLayer::Signal { signals } => DeepFusionLayer::Signal {
                        signals: signals.into_iter().map(&mut map).collect(),
                    },
                    other => other,
                })
                .collect(),
            alpha,
            gating,
        },
        OperatorTree::Opaque {
            kind,
            children,
            meta,
        } => OperatorTree::Opaque {
            kind,
            children: children.into_iter().map(&mut map).collect(),
            meta,
        },
        OperatorTree::Empty => OperatorTree::Empty,
        OperatorTree::Term {
            query,
            field,
            scoring,
            top_k,
        } => OperatorTree::Term {
            query,
            field,
            scoring,
            top_k,
        },
        OperatorTree::BayesianMatchWithPrior {
            field,
            query,
            prior_field,
            mode,
        } => OperatorTree::BayesianMatchWithPrior {
            field,
            query,
            prior_field,
            mode,
        },
        OperatorTree::VectorSimilarity {
            query_vector,
            threshold,
            field,
        } => OperatorTree::VectorSimilarity {
            query_vector,
            threshold,
            field,
        },
        OperatorTree::KNN {
            query_vector,
            k,
            field,
        } => OperatorTree::KNN {
            query_vector,
            k,
            field,
        },
        OperatorTree::CalibratedVectorMatch {
            query_vector,
            k,
            field,
            threshold,
        } => OperatorTree::CalibratedVectorMatch {
            query_vector,
            k,
            field,
            threshold,
        },
        OperatorTree::Traverse {
            start_vertex,
            graph,
            label,
            max_hops,
            vertex_predicate,
        } => OperatorTree::Traverse {
            start_vertex,
            graph,
            label,
            max_hops,
            vertex_predicate,
        },
        OperatorTree::GraphNeighbors {
            vertex,
            graph,
            label,
            direction,
        } => OperatorTree::GraphNeighbors {
            vertex,
            graph,
            label,
            direction,
        },
        OperatorTree::GraphEdges { graph, label } => OperatorTree::GraphEdges { graph, label },
        OperatorTree::PatternMatch { pattern, graph } => {
            OperatorTree::PatternMatch { pattern, graph }
        }
        OperatorTree::RegularPathQuery {
            rpq_source,
            start_vertex,
            graph,
        } => OperatorTree::RegularPathQuery {
            rpq_source,
            start_vertex,
            graph,
        },
        OperatorTree::IndexScan {
            index_name,
            field,
            predicate,
        } => OperatorTree::IndexScan {
            index_name,
            field,
            predicate,
        },
        OperatorTree::MultiFieldSearch {
            fields,
            queries,
            weights,
        } => OperatorTree::MultiFieldSearch {
            fields,
            queries,
            weights,
        },
        OperatorTree::WeightedPathQuery {
            rpq_source,
            start_vertex,
            graph,
            weight_property,
            default_edge_weight,
            max_hops,
            predicate,
            predicate_selectivity,
            score,
        } => OperatorTree::WeightedPathQuery {
            rpq_source,
            start_vertex,
            graph,
            weight_property,
            default_edge_weight,
            max_hops,
            predicate,
            predicate_selectivity,
            score,
        },
        OperatorTree::PageRank { graph } => OperatorTree::PageRank { graph },
        OperatorTree::HITS { graph } => OperatorTree::HITS { graph },
        OperatorTree::BetweennessCentrality { graph } => {
            OperatorTree::BetweennessCentrality { graph }
        }
        OperatorTree::TemporalTraverse {
            start_vertex,
            graph,
            label,
            max_hops,
            temporal_filter,
        } => OperatorTree::TemporalTraverse {
            start_vertex,
            graph,
            label,
            max_hops,
            temporal_filter,
        },
        OperatorTree::TemporalPatternMatch {
            pattern,
            graph,
            temporal_filter,
        } => OperatorTree::TemporalPatternMatch {
            pattern,
            graph,
            temporal_filter,
        },
        OperatorTree::DeepPredict { model } => OperatorTree::DeepPredict { model },
    }
}
