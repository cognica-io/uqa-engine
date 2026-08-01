//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exhaustive physical dispatch for every operator-tree variant.

use super::{
    sparse_threshold_inline, DriverResult, EngineDriver, OperatorOutput, OperatorTree,
    OperatorTreeDriver, PositiveEvidencePoolExecution, PostingList, SQLError,
    WeightedPathExecution,
};

impl OperatorTreeDriver for EngineDriver<'_> {
    type Error = SQLError;

    // Keep one exhaustive physical-dispatch match: adding an IR variant must
    // fail compilation here instead of falling through a category wildcard.
    #[allow(clippy::match_same_arms)]
    #[allow(clippy::too_many_lines)]
    fn execute_node(&self, op: &OperatorTree) -> DriverResult<OperatorOutput> {
        let posting = match op {
            OperatorTree::Empty => Ok(PostingList::new()),
            OperatorTree::Term {
                query,
                field,
                scoring,
                top_k,
            } => self.execute_term(query, field.as_deref(), *scoring, *top_k),
            OperatorTree::BayesianScore { source, field } => {
                self.execute_bayesian_score(source, field.as_deref())
            }
            OperatorTree::BayesianMatchWithPrior {
                field,
                query,
                prior_field,
                mode,
            } => self.execute_bayesian_match_with_prior(field, query, prior_field, *mode),
            OperatorTree::KNN {
                query_vector,
                k,
                field,
            } => self.execute_knn(query_vector, *k, field),
            OperatorTree::CalibratedVectorMatch {
                query_vector,
                k,
                field,
                threshold,
            } => self.execute_calibrated_vector_match(field, query_vector, *k, *threshold),
            OperatorTree::Filter {
                field,
                predicate,
                source,
            } => self.execute_filter(field, predicate, source.as_deref()),
            OperatorTree::Facet { field, source } => self.execute_facet(field, source.as_deref()),
            OperatorTree::Score {
                scorer,
                source,
                query_terms,
                field,
            } => self.execute_score(scorer, source, query_terms, field),
            OperatorTree::Intersect(parts) => return self.execute_intersect(parts),
            OperatorTree::Union(parts) => return self.execute_union(parts),
            OperatorTree::Complement(inner) => self.execute_complement(inner),
            OperatorTree::Composed(parts) => return self.execute_composed(parts),
            OperatorTree::EncodeGraphPosting { source } => {
                return match self.execute_node(source)? {
                    OperatorOutput::Graph(result) => Ok(OperatorOutput::Posting(
                        uqa_graph::GraphPostingCodec::encode(result),
                    )),
                    OperatorOutput::Posting(_) => Err(SQLError::TypeMismatch(
                        "EncodeGraphPosting requires a graph posting carrier".to_string(),
                    )),
                    OperatorOutput::Generalized(_) => Err(SQLError::TypeMismatch(
                        "EncodeGraphPosting cannot encode a tuple carrier".to_string(),
                    )),
                };
            }
            OperatorTree::VectorSimilarity {
                query_vector,
                threshold,
                field,
            } => self.execute_vector_similarity(query_vector, *threshold, field),
            OperatorTree::BayesianEvidenceFusion { signals, base_rate } => {
                self.execute_bayesian_evidence_fusion(signals, *base_rate)
            }
            OperatorTree::RobustPositiveEvidencePool {
                signals,
                alpha,
                gating,
                weights,
                logit_min,
                logit_max,
                adaptive_weights,
            } => self.execute_positive_evidence_pool(PositiveEvidencePoolExecution {
                signals,
                alpha: *alpha,
                gating,
                weights: weights.as_deref(),
                logit_min: logit_min.as_deref(),
                logit_max: logit_max.as_deref(),
                adaptive_weights: *adaptive_weights,
            }),
            OperatorTree::ProbBoolFusion { signals, mode } => {
                self.execute_prob_bool_fusion(signals, *mode)
            }
            OperatorTree::ProbNot {
                signal,
                default_prob,
            } => self.execute_prob_not(signal, *default_prob),
            OperatorTree::IndexScan {
                index_name,
                field,
                predicate,
            } => self.execute_index_scan(index_name, field, predicate),
            OperatorTree::Aggregate {
                source,
                field,
                monoid,
            } => self.execute_aggregate(source.as_deref(), field, monoid),
            OperatorTree::GroupBy {
                source,
                group_field,
                agg_field,
                monoid,
            } => self.execute_group_by(source, group_field, agg_field, monoid),
            OperatorTree::HybridTextVector {
                term_op,
                vector_op,
                alpha,
            } => self.execute_hybrid_text_vector(term_op, vector_op, *alpha),
            OperatorTree::SemanticFilter { source, vector_op } => {
                self.execute_semantic_filter(source, vector_op)
            }
            OperatorTree::VectorExclusion { positive, negative } => {
                self.execute_vector_exclusion(positive, negative)
            }
            OperatorTree::FacetVector {
                vector_op,
                facet_field,
            } => self.execute_facet_vector(vector_op, facet_field),
            OperatorTree::CosineProbability(source) => self.execute_cosine_probability(source),
            OperatorTree::AttentionFusion {
                signals,
                attention,
                query_features,
            } => self.execute_attention_fusion(signals, attention, query_features),
            OperatorTree::LearnedFusion { signals, learned } => {
                self.execute_learned_fusion(signals, learned)
            }
            OperatorTree::SparseThreshold { source, threshold } => {
                let source = self.execute_posting_node(source)?;
                sparse_threshold_inline(&source, *threshold)
            }
            OperatorTree::MultiFieldSearch {
                fields,
                queries,
                weights,
            } => self.execute_multi_field_search(fields, queries, weights.as_deref()),
            OperatorTree::MultiStage { stages } => self.execute_multi_stage(stages),
            OperatorTree::Traverse {
                start_vertex,
                graph,
                label,
                max_hops,
                vertex_predicate,
            } => {
                return self
                    .execute_traverse(
                        *start_vertex,
                        graph,
                        label.as_deref(),
                        *max_hops,
                        vertex_predicate.as_ref(),
                    )
                    .map(OperatorOutput::Graph);
            }
            OperatorTree::GraphNeighbors {
                vertex,
                graph,
                label,
                direction,
            } => self.execute_graph_neighbors(*vertex, graph, label.as_deref(), *direction),
            OperatorTree::GraphEdges { graph, label } => {
                self.execute_graph_edges(graph, label.as_deref())
            }
            OperatorTree::PatternMatch { pattern, graph } => {
                return self
                    .execute_pattern_match(pattern, graph)
                    .map(OperatorOutput::Graph);
            }
            OperatorTree::RegularPathQuery {
                rpq_source,
                start_vertex,
                graph,
            } => {
                return self
                    .execute_regular_path_query(rpq_source, *start_vertex, graph)
                    .map(OperatorOutput::Graph);
            }
            OperatorTree::GraphJoin {
                left,
                right,
                label,
                graph,
            } => {
                return self
                    .execute_graph_join(left, right, label.as_deref(), graph)
                    .map(OperatorOutput::Generalized);
            }
            OperatorTree::VertexAggregation { source, monoid } => {
                self.execute_vertex_aggregation(source, monoid)
            }
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
            } => {
                return self
                    .execute_weighted_path_query(WeightedPathExecution {
                        rpq_source,
                        start_vertex: *start_vertex,
                        graph,
                        weight_property,
                        default_edge_weight: *default_edge_weight,
                        max_hops: *max_hops,
                        predicate,
                        predicate_selectivity: *predicate_selectivity,
                        score: *score,
                    })
                    .map(OperatorOutput::Graph);
            }
            OperatorTree::MessagePassing { source } => {
                return self
                    .execute_message_passing(source)
                    .map(OperatorOutput::Graph);
            }
            OperatorTree::GraphEmbedding { source } => {
                return self
                    .execute_graph_embedding(source)
                    .map(OperatorOutput::Graph);
            }
            OperatorTree::PageRank { graph } => {
                return self.execute_page_rank(graph).map(OperatorOutput::Graph);
            }
            OperatorTree::HITS { graph } => {
                return self.execute_hits(graph).map(OperatorOutput::Graph);
            }
            OperatorTree::BetweennessCentrality { graph } => {
                return self
                    .execute_betweenness_centrality(graph)
                    .map(OperatorOutput::Graph);
            }
            OperatorTree::TextSimilarityJoin {
                left,
                right,
                threshold,
            } => {
                return self
                    .execute_text_similarity_join(left, right, *threshold)
                    .map(OperatorOutput::Generalized);
            }
            OperatorTree::VectorSimilarityJoin {
                left,
                right,
                threshold,
            } => {
                return self
                    .execute_vector_similarity_join(left, right, *threshold)
                    .map(OperatorOutput::Generalized);
            }
            OperatorTree::HybridJoin { left, right } => {
                return self
                    .execute_hybrid_join(left, right)
                    .map(OperatorOutput::Generalized);
            }
            OperatorTree::CrossParadigmJoin { left, right } => {
                return self
                    .execute_cross_paradigm_join(left, right)
                    .map(OperatorOutput::Generalized);
            }
            OperatorTree::TemporalTraverse {
                start_vertex,
                graph,
                label,
                max_hops,
                temporal_filter,
            } => {
                return self
                    .execute_temporal_traverse(
                        *start_vertex,
                        graph,
                        label.as_deref(),
                        *max_hops,
                        temporal_filter.as_ref(),
                    )
                    .map(OperatorOutput::Graph);
            }
            OperatorTree::TemporalPatternMatch {
                pattern,
                graph,
                temporal_filter,
            } => {
                return self
                    .execute_temporal_pattern_match(pattern, graph, temporal_filter.as_ref())
                    .map(OperatorOutput::Graph);
            }
            OperatorTree::ProgressiveFusion {
                stages,
                alpha,
                gating,
            } => self.execute_progressive_fusion(stages, *alpha, gating),
            OperatorTree::DeepFusion {
                layers,
                alpha,
                gating,
            } => self.execute_deep_fusion(layers, *alpha, gating),
            OperatorTree::DeepPredict { model } => self.execute_deep_predict(model),
            OperatorTree::Opaque {
                kind,
                children,
                meta,
            } => Self::execute_opaque(kind, children, meta),
        }?;
        Ok(OperatorOutput::Posting(posting))
    }
}
