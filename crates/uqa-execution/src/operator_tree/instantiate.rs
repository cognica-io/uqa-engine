//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Instantiate physical retrieval models after SQL has produced a bound logical expression.

use std::sync::Arc;
use uqa_fusion::{AttentionFusion, LearnedFusion, MultiHeadAttentionFusion, N_QUERY_FEATURES};
use uqa_operators::{MultiStageEntry, OperatorTree, TextScoringMode};
use uqa_sql::{
    retrieval::{AttentionSpec, RetrievalExpr, TextScoringMode as LogicalTextScoring},
    SQLError,
};

#[expect(
    clippy::too_many_lines,
    reason = "preserves exhaustive logical-to-physical variant mapping"
)]
pub fn instantiate(expression: RetrievalExpr) -> Result<OperatorTree, SQLError> {
    Ok(match expression {
        RetrievalExpr::Empty => OperatorTree::Empty,
        RetrievalExpr::BayesianMatchWithPrior {
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
        RetrievalExpr::KNN {
            query_vector,
            k,
            field,
        } => OperatorTree::KNN {
            query_vector,
            k,
            field,
        },
        RetrievalExpr::CalibratedVectorMatch {
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
        RetrievalExpr::GraphNeighbors {
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
        RetrievalExpr::GraphEdges { graph, label } => OperatorTree::GraphEdges { graph, label },
        RetrievalExpr::RegularPathQuery {
            rpq_source,
            start_vertex,
            graph,
        } => OperatorTree::RegularPathQuery {
            rpq_source,
            start_vertex,
            graph,
        },
        RetrievalExpr::TemporalTraverse {
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
        RetrievalExpr::PageRank { graph } => OperatorTree::PageRank { graph },
        RetrievalExpr::HITS { graph } => OperatorTree::HITS { graph },
        RetrievalExpr::BetweennessCentrality { graph } => {
            OperatorTree::BetweennessCentrality { graph }
        }
        RetrievalExpr::DeepPredict { model } => OperatorTree::DeepPredict { model },
        RetrievalExpr::MultiFieldSearch {
            fields,
            queries,
            weights,
        } => OperatorTree::MultiFieldSearch {
            fields,
            queries,
            weights,
        },
        RetrievalExpr::Term {
            query,
            field,
            scoring,
        } => OperatorTree::Term {
            query,
            field,
            scoring: scoring.map(|mode| match mode {
                LogicalTextScoring::BM25 => TextScoringMode::BM25,
                LogicalTextScoring::BayesianBM25 => TextScoringMode::BayesianBM25,
            }),
            top_k: None,
        },
        RetrievalExpr::Filter {
            field,
            predicate,
            source,
        } => OperatorTree::Filter {
            field,
            predicate,
            source: source
                .map(|child| instantiate(*child).map(Box::new))
                .transpose()?,
        },
        RetrievalExpr::Traverse {
            start_vertex,
            graph,
            label,
            max_hops,
        } => OperatorTree::Traverse {
            start_vertex,
            graph,
            label,
            max_hops,
            vertex_predicate: None,
        },
        RetrievalExpr::MultiStage { stages } => OperatorTree::MultiStage {
            stages: stages
                .into_iter()
                .map(|stage| {
                    Ok(MultiStageEntry {
                        child: instantiate(stage.child)?,
                        cutoff: stage.cutoff,
                    })
                })
                .collect::<Result<_, SQLError>>()?,
        },
        RetrievalExpr::AttentionFusion {
            signals,
            options,
            function_name,
        } => {
            let signals = instantiate_all(signals)?;
            let attention: uqa_operators::AttentionRef = match options {
                AttentionSpec::MultiHead {
                    n_heads,
                    alpha,
                    normalized,
                } => Arc::new(
                    MultiHeadAttentionFusion::try_new(
                        n_heads,
                        signals.len(),
                        N_QUERY_FEATURES,
                        alpha,
                        normalized,
                    )
                    .map_err(|error| SQLError::TypeMismatch(format!("{function_name}: {error}")))?,
                ),
                AttentionSpec::Single {
                    alpha,
                    normalized,
                    base_rate,
                } => Arc::new(
                    AttentionFusion::new(signals.len(), N_QUERY_FEATURES, alpha)
                        .with_options(normalized, base_rate)
                        .map_err(|error| {
                            SQLError::TypeMismatch(format!("{function_name}: {error}"))
                        })?,
                ),
            };
            OperatorTree::AttentionFusion {
                signals,
                attention,
                query_features: Vec::new(),
            }
        }
        RetrievalExpr::LearnedFusion { signals, alpha } => {
            let signals = instantiate_all(signals)?;
            let learned = Arc::new(LearnedFusion::new(signals.len(), alpha));
            OperatorTree::LearnedFusion { signals, learned }
        }
        RetrievalExpr::Intersect(children) => OperatorTree::Intersect(instantiate_all(children)?),
        RetrievalExpr::Union(children) => OperatorTree::Union(instantiate_all(children)?),
        RetrievalExpr::Composed(children) => OperatorTree::Composed(instantiate_all(children)?),
        RetrievalExpr::Complement(child) => {
            OperatorTree::Complement(Box::new(instantiate(*child)?))
        }
        RetrievalExpr::CosineProbability(child) => {
            OperatorTree::CosineProbability(Box::new(instantiate(*child)?))
        }
        RetrievalExpr::BayesianScore { source, field } => OperatorTree::BayesianScore {
            source: Box::new(instantiate(*source)?),
            field,
        },
        RetrievalExpr::SparseThreshold { source, threshold } => OperatorTree::SparseThreshold {
            source: Box::new(instantiate(*source)?),
            threshold,
        },
        RetrievalExpr::EncodeGraphPosting { source } => OperatorTree::EncodeGraphPosting {
            source: Box::new(instantiate(*source)?),
        },
        RetrievalExpr::BayesianEvidenceFusion { signals, base_rate } => {
            OperatorTree::BayesianEvidenceFusion {
                signals: instantiate_all(signals)?,
                base_rate,
            }
        }
        RetrievalExpr::RobustPositiveEvidencePool {
            signals,
            alpha,
            gating,
            weights,
            logit_min,
            logit_max,
            adaptive_weights,
        } => OperatorTree::RobustPositiveEvidencePool {
            signals: instantiate_all(signals)?,
            alpha,
            gating,
            weights,
            logit_min,
            logit_max,
            adaptive_weights,
        },
        RetrievalExpr::TextSimilarityJoin {
            left,
            right,
            threshold,
        } => OperatorTree::TextSimilarityJoin {
            left: Box::new(instantiate(*left)?),
            right: Box::new(instantiate(*right)?),
            threshold,
        },
        RetrievalExpr::VectorSimilarityJoin {
            left,
            right,
            threshold,
        } => OperatorTree::VectorSimilarityJoin {
            left: Box::new(instantiate(*left)?),
            right: Box::new(instantiate(*right)?),
            threshold,
        },
        RetrievalExpr::GraphJoin {
            left,
            right,
            label,
            graph,
        } => OperatorTree::GraphJoin {
            left: Box::new(instantiate(*left)?),
            right: Box::new(instantiate(*right)?),
            label,
            graph,
        },
        RetrievalExpr::HybridJoin { left, right } => OperatorTree::HybridJoin {
            left: Box::new(instantiate(*left)?),
            right: Box::new(instantiate(*right)?),
        },
        RetrievalExpr::CrossParadigmJoin { left, right } => OperatorTree::CrossParadigmJoin {
            left: Box::new(instantiate(*left)?),
            right: Box::new(instantiate(*right)?),
        },
    })
}
fn instantiate_all(expressions: Vec<RetrievalExpr>) -> Result<Vec<OperatorTree>, SQLError> {
    expressions.into_iter().map(instantiate).collect()
}
