//! Fusion, retrieval, graph, and cross-paradigm operator heuristics.

use super::{
    CardinalityEstimator, DeepFusionLayer, IndexStats, MultiStageCutoff, OperatorTree,
    ProbBoolMode, GRAPH_AVG_DEGREE_DEFAULT, JACCARD_JOIN_SELECTIVITY, VECTOR_JOIN_SELECTIVITY,
};

impl CardinalityEstimator {
    /// Cardinality estimation for cross-paradigm operators. Mirrors
    /// `_estimate_cross_paradigm`.
    pub(super) fn estimate_cross_paradigm(
        &self,
        op: &OperatorTree,
        stats: &IndexStats,
        n: f64,
    ) -> f64 {
        match op {
            OperatorTree::MultiStage { stages } => {
                if let Some(last) = stages.last() {
                    match last.cutoff {
                        MultiStageCutoff::TopK(k) => k as f64,
                        MultiStageCutoff::Ratio(r) => n * r,
                    }
                } else {
                    n * 0.5
                }
            }

            OperatorTree::AttentionFusion { signals, .. }
            | OperatorTree::LearnedFusion { signals, .. }
            | OperatorTree::BayesianEvidenceFusion { signals, .. }
            | OperatorTree::RobustPositiveEvidencePool { signals, .. } => {
                let sum: f64 = signals.iter().map(|s| self.estimate(s, stats)).sum();
                n.min(sum)
            }

            OperatorTree::MultiFieldSearch { fields, .. } => n.min(n * 0.3 * fields.len() as f64),

            OperatorTree::SparseThreshold { source, .. } => self.estimate(source, stats) * 0.5,

            OperatorTree::VectorExclusion { positive, negative } => {
                let pos = self.estimate(positive, stats);
                let neg = self.estimate(negative, stats);
                let overlap = if n > 0.0 { (pos * neg) / n } else { 0.0 };
                (pos - overlap).max(1.0)
            }

            OperatorTree::FacetVector { vector_op, .. } => self.estimate(vector_op, stats),

            OperatorTree::VertexAggregation { .. } => 1.0,

            OperatorTree::ProbBoolFusion { signals, mode } => {
                let cards: Vec<f64> = signals.iter().map(|s| self.estimate(s, stats)).collect();
                match mode {
                    ProbBoolMode::And => {
                        if cards.is_empty() {
                            0.0
                        } else {
                            let mut result = cards[0];
                            for c in &cards[1..] {
                                if n > 0.0 {
                                    result = (result * c) / n;
                                }
                            }
                            result.max(1.0)
                        }
                    }
                    ProbBoolMode::Or => n.min(cards.iter().sum()),
                }
            }

            OperatorTree::ProbNot { signal, .. } => {
                let inner = self.estimate(signal, stats);
                (n - inner).max(0.0)
            }

            OperatorTree::HybridTextVector {
                term_op, vector_op, ..
            } => {
                let text = self.estimate(term_op, stats);
                let vec_card = self.estimate(vector_op, stats);
                if n > 0.0 {
                    ((text * vec_card) / n).max(1.0)
                } else {
                    1.0
                }
            }

            OperatorTree::SemanticFilter { source, vector_op } => {
                let src = self.estimate(source, stats);
                let vec_card = self.estimate(vector_op, stats);
                if n > 0.0 {
                    ((src * vec_card) / n).max(1.0)
                } else {
                    1.0
                }
            }

            OperatorTree::TemporalTraverse {
                label,
                max_hops,
                temporal_filter,
                ..
            } => self.estimate_traverse(label.as_deref(), *max_hops, n, temporal_filter.as_ref()),

            OperatorTree::TemporalPatternMatch {
                pattern,
                temporal_filter,
                ..
            } => self.estimate_temporal_pattern_match(pattern, temporal_filter.as_ref(), n),

            OperatorTree::Traverse {
                label, max_hops, ..
            } => self.estimate_traverse(label.as_deref(), *max_hops, n, None),

            OperatorTree::GraphNeighbors { label, .. } => {
                let degree = self
                    .graph_stats
                    .as_ref()
                    .map(|stats| stats.avg_out_degree * stats.label_selectivity(label.as_deref()))
                    .unwrap_or(GRAPH_AVG_DEGREE_DEFAULT);
                degree.min(n).max(0.0)
            }

            OperatorTree::GraphEdges { label, .. } => self
                .graph_stats
                .as_ref()
                .map(|stats| stats.num_edges as f64 * stats.label_selectivity(label.as_deref()))
                .unwrap_or(n),

            OperatorTree::PatternMatch { pattern, .. } => self.estimate_pattern_match(pattern, n),

            OperatorTree::RegularPathQuery { rpq_source, .. } => self.estimate_rpq(rpq_source, n),

            OperatorTree::WeightedPathQuery {
                rpq_source,
                predicate_selectivity,
                ..
            } => self.estimate_rpq(rpq_source, n) * predicate_selectivity,

            OperatorTree::MessagePassing { .. } | OperatorTree::GraphEmbedding { .. } => n,

            OperatorTree::PageRank { .. }
            | OperatorTree::HITS { .. }
            | OperatorTree::BetweennessCentrality { .. } => self
                .graph_stats
                .as_ref()
                .map(|gs| gs.num_vertices as f64)
                .unwrap_or(n),

            OperatorTree::TextSimilarityJoin { left, right, .. } => {
                let l = self.estimate_join_side(left, stats, n);
                let r = self.estimate_join_side(right, stats, n);
                l * r * JACCARD_JOIN_SELECTIVITY
            }

            OperatorTree::VectorSimilarityJoin { left, right, .. } => {
                let l = self.estimate_join_side(left, stats, n);
                let r = self.estimate_join_side(right, stats, n);
                l * r * VECTOR_JOIN_SELECTIVITY
            }

            OperatorTree::GraphJoin { left, label, .. } => {
                let l = self.estimate_join_side(left, stats, n);
                let avg_degree = self
                    .graph_stats
                    .as_ref()
                    .map(|gs| gs.avg_out_degree)
                    .unwrap_or(GRAPH_AVG_DEGREE_DEFAULT);
                let label_sel = self
                    .graph_stats
                    .as_ref()
                    .map(|gs| gs.label_selectivity(label.as_deref()))
                    .unwrap_or(1.0);
                l * avg_degree * label_sel
            }

            OperatorTree::HybridJoin { left, right } => {
                let l = self.estimate_join_side(left, stats, n);
                let r = self.estimate_join_side(right, stats, n);
                if n > 0.0 {
                    (l * r) / n
                } else {
                    0.0
                }
            }

            OperatorTree::CrossParadigmJoin { left, .. } => {
                let l = self.estimate_join_side(left, stats, n);
                let avg_degree = self
                    .graph_stats
                    .as_ref()
                    .map(|gs| gs.avg_out_degree)
                    .unwrap_or(GRAPH_AVG_DEGREE_DEFAULT);
                let label_sel = 1.0;
                l * avg_degree * label_sel
            }

            OperatorTree::ProgressiveFusion { stages, .. } => {
                stages.last().map(|s| s.k as f64).unwrap_or(n)
            }

            OperatorTree::DeepFusion { layers, .. } => self.estimate_deep_fusion(layers, stats, n),

            OperatorTree::CosineProbability(inner) => self.estimate(inner, stats),
            OperatorTree::BayesianScore { source, .. } => self.estimate(source, stats),
            OperatorTree::EncodeGraphPosting { source } => self.estimate(source, stats),
            OperatorTree::BayesianMatchWithPrior { field, query, .. } => {
                if stats.total_docs == 0 {
                    0.0
                } else {
                    stats.doc_freq(field, query) as f64
                }
            }
            OperatorTree::CalibratedVectorMatch { k, .. } => n.min(*k as f64),
            OperatorTree::DeepPredict { .. } => n,

            OperatorTree::Composed(ops) => {
                ops.last().map(|o| self.estimate(o, stats)).unwrap_or(0.0)
            }

            OperatorTree::Facet { .. } => n,
            OperatorTree::IndexScan {
                field, predicate, ..
            } => n * self.filter_selectivity(field, predicate, n),
            OperatorTree::Aggregate { .. } => 1.0,
            OperatorTree::GroupBy { .. } => n * 0.1,

            OperatorTree::Opaque { children, .. } => children
                .iter()
                .map(|c| self.estimate(c, stats))
                .fold(0.0, f64::max),

            // Variants already handled in `estimate`.
            OperatorTree::Empty
            | OperatorTree::Term { .. }
            | OperatorTree::Filter { .. }
            | OperatorTree::Score { .. }
            | OperatorTree::Intersect(_)
            | OperatorTree::Union(_)
            | OperatorTree::Complement(_)
            | OperatorTree::VectorSimilarity { .. }
            | OperatorTree::KNN { .. } => n,
        }
    }

    fn estimate_deep_fusion(&self, layers: &[DeepFusionLayer], stats: &IndexStats, n: f64) -> f64 {
        let mut card: f64 = 0.0;
        for layer in layers {
            match layer {
                DeepFusionLayer::Signal { signals } => {
                    let sum: f64 = signals.iter().map(|s| self.estimate(s, stats)).sum();
                    card = card.max(n.min(sum));
                }
                DeepFusionLayer::Propagate { edge_label, .. } => {
                    let avg_degree = self
                        .graph_stats
                        .as_ref()
                        .map(|gs| gs.avg_out_degree)
                        .unwrap_or(GRAPH_AVG_DEGREE_DEFAULT);
                    let label_sel = self
                        .graph_stats
                        .as_ref()
                        .map(|gs| gs.label_selectivity(edge_label.as_deref()))
                        .unwrap_or(1.0);
                    card = n.min(card * avg_degree * label_sel);
                }
                DeepFusionLayer::Conv { .. } => {}
                DeepFusionLayer::Pool { pool_size, .. } => {
                    let denom = (*pool_size).max(1) as f64;
                    card = (card / denom).max(1.0);
                }
                DeepFusionLayer::Flatten => {
                    card = 1.0;
                }
                DeepFusionLayer::Dense { .. }
                | DeepFusionLayer::Softmax
                | DeepFusionLayer::BatchNorm { .. }
                | DeepFusionLayer::Dropout { .. } => {}
            }
        }
        card.max(1.0)
    }
}
