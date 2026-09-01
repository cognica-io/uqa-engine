//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Graph traversal, pattern, centrality, and temporal execution.

use super::{
    graph_execution_error, graph_pattern_from_ir, numeric_score, operator_execution_error,
    parse_rpq, require_graph_name, restrict_result_to_source, temporal_filter_from_ir, BTreeMap,
    BTreeSet, DeepGraphDirection, DriverResult, EngineDriver, GeneralizedPostingList, OperatorTree,
    Payload, PostingEntry, PostingList, SQLError, Value, WeightedPathExecution,
};

impl EngineDriver<'_> {
    pub(super) fn execute_traverse(
        &self,
        start_vertex: u64,
        graph: &str,
        label: Option<&str>,
        max_hops: usize,
        vertex_predicate: Option<&uqa_operators::VertexPredicate>,
    ) -> DriverResult<uqa_graph::GraphPostingList> {
        let max_hops = u32::try_from(max_hops).map_err(|_| {
            SQLError::TypeMismatch(format!("Traverse.max_hops is too large: {max_hops}"))
        })?;
        self.with_graph(graph, |store| {
            let mut op = uqa_graph::Traverse::new(start_vertex, graph).max_hops(max_hops);
            if let Some(label) = label {
                op = op.label(label);
            }
            if let Some(predicate) = vertex_predicate {
                op = op.predicate(uqa_graph::VertexPredicate::Custom(predicate.clone()));
            }
            op.execute(store)
                .map_err(|error| graph_execution_error("Traverse", error))
        })
    }

    pub(super) fn execute_graph_neighbors(
        &self,
        vertex: u64,
        graph: &str,
        label: Option<&str>,
        direction: DeepGraphDirection,
    ) -> DriverResult<PostingList> {
        let direction = match direction {
            DeepGraphDirection::Out => uqa_graph::Direction::Out,
            DeepGraphDirection::In => uqa_graph::Direction::In,
            DeepGraphDirection::Both => uqa_graph::Direction::Both,
        };
        let neighbors = self.with_graph(graph, |store| {
            <uqa_graph::MemoryGraphStore as uqa_graph::GraphStore>::neighbors(
                store, vertex, label, direction, graph,
            )
            .map_err(|error| graph_execution_error("GraphNeighbors", error))
        })?;
        let entries = neighbors
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|doc_id| {
                PostingEntry::new(
                    doc_id,
                    Payload {
                        score: 1.0,
                        ..Default::default()
                    },
                )
            })
            .collect();
        Ok(PostingList::from_sorted_unchecked(entries))
    }

    pub(super) fn execute_graph_edges(
        &self,
        graph: &str,
        label: Option<&str>,
    ) -> DriverResult<PostingList> {
        let edges = self.with_graph(graph, |store| {
            <uqa_graph::MemoryGraphStore as uqa_graph::GraphStore>::edges_in_graph(store, graph)
                .map_err(|error| graph_execution_error("GraphEdges", error))
        })?;
        let mut entries = Vec::new();
        for edge in edges {
            if label.is_some_and(|label| edge.label != label) {
                continue;
            }
            let score = match edge.properties.get("weight") {
                Some(Value::Float(value)) => *value,
                Some(Value::Int(value)) => *value as f64,
                Some(Value::Decimal(value)) => value.to_f64().ok_or_else(|| {
                    SQLError::TypeMismatch("GraphEdges.weight decimal is outside f64 range".into())
                })?,
                _ => 1.0,
            };
            entries.push(PostingEntry::new(
                edge.edge_id,
                Payload {
                    score,
                    ..Default::default()
                },
            ));
        }
        Ok(PostingList::from_unsorted(entries))
    }

    pub(super) fn execute_pattern_match(
        &self,
        pattern: &uqa_operators::GraphPatternIR,
        graph: &str,
    ) -> DriverResult<uqa_graph::GraphPostingList> {
        let pattern = graph_pattern_from_ir(pattern);
        self.with_graph(graph, |store| {
            uqa_graph::GMatch::new(pattern, graph)
                .execute(store)
                .map_err(|error| graph_execution_error("PatternMatch", error))
        })
    }

    pub(super) fn execute_regular_path_query(
        &self,
        rpq_source: &str,
        start_vertex: u64,
        graph: &str,
    ) -> DriverResult<uqa_graph::GraphPostingList> {
        let path = parse_rpq(rpq_source)?;
        self.with_graph(graph, |store| {
            uqa_graph::RegularPathQuery::new(path, graph)
                .from_vertex(start_vertex)
                .execute(store)
                .map_err(|error| graph_execution_error("RegularPathQuery", error))
        })
    }

    pub(super) fn execute_graph_join(
        &self,
        left: &OperatorTree,
        right: &OperatorTree,
        label: Option<&str>,
        graph: &str,
    ) -> DriverResult<GeneralizedPostingList> {
        let left = self.execute_posting_node(left)?;
        let right = self.execute_posting_node(right)?;
        self.join_graph_postings(&left, &right, label, graph)
    }

    pub(super) fn join_graph_postings(
        &self,
        left: &PostingList,
        right: &PostingList,
        label: Option<&str>,
        graph: &str,
    ) -> DriverResult<GeneralizedPostingList> {
        self.with_graph(graph, |store| {
            let mut op = uqa_joins::GraphJoin::new(left.entries(), right.entries(), store, graph);
            if let Some(label) = label {
                op = op.label(label);
            }
            op.execute()
                .map_err(|error| graph_execution_error("GraphJoin", error))
        })
    }

    pub(super) fn execute_vertex_aggregation(
        &self,
        source: &OperatorTree,
        monoid: &std::sync::Arc<dyn uqa_operators::AggregationMonoid>,
    ) -> DriverResult<PostingList> {
        let source = self.execute_posting_node(source)?;
        let mut state = monoid.identity();
        for entry in source.entries() {
            state = monoid
                .accumulate(state, &Value::Float(entry.payload.score))
                .map_err(|error| operator_execution_error("VertexAggregation", error))?;
        }
        let result = monoid
            .finalize(state)
            .map_err(|error| operator_execution_error("VertexAggregation", error))?;
        let score = numeric_score(&result);
        let mut fields = BTreeMap::new();
        fields.insert("_vertex_aggregate".to_string(), result);
        fields.insert(
            "_vertex_aggregate_count".to_string(),
            Value::Int(i64::try_from(source.len()).map_err(|_| {
                SQLError::Internal(format!(
                    "vertex aggregate input count {} exceeds the SQL BIGINT range",
                    source.len()
                ))
            })?),
        );
        Ok(PostingList::from_sorted_unchecked(vec![PostingEntry::new(
            0,
            Payload {
                score,
                fields,
                ..Default::default()
            },
        )]))
    }

    pub(super) fn execute_weighted_path_query(
        &self,
        query: WeightedPathExecution<'_>,
    ) -> DriverResult<uqa_graph::GraphPostingList> {
        let WeightedPathExecution {
            rpq_source,
            start_vertex,
            graph,
            weight_property,
            default_edge_weight,
            max_hops,
            predicate,
            predicate_selectivity,
            score,
        } = query;
        if !predicate_selectivity.is_finite() || !(0.0..=1.0).contains(&predicate_selectivity) {
            return Err(SQLError::TypeMismatch(format!(
                "WeightedPathQuery.predicate_selectivity must be finite and in [0, 1], got {predicate_selectivity}"
            )));
        }
        if weight_property.is_empty() {
            return Err(SQLError::TypeMismatch(
                "WeightedPathQuery.weight_property must not be empty".to_string(),
            ));
        }
        if !default_edge_weight.is_finite() {
            return Err(SQLError::TypeMismatch(format!(
                "WeightedPathQuery.default_edge_weight must be finite, got {default_edge_weight}"
            )));
        }
        if !score.is_finite() {
            return Err(SQLError::TypeMismatch(format!(
                "WeightedPathQuery.score must be finite, got {score}"
            )));
        }
        let path = parse_rpq(rpq_source)?;
        self.with_graph(graph, |store| {
            let mut op = uqa_graph::WeightedPathQuery::new(
                path,
                graph,
                weight_property,
                std::sync::Arc::clone(predicate),
            )
            .from_vertex(start_vertex);
            op.default_edge_weight = default_edge_weight;
            op.max_hops = max_hops;
            op.score = score;
            op.execute(store)
                .map_err(|error| graph_execution_error("WeightedPathQuery", error))
        })
    }

    pub(super) fn execute_message_passing(
        &self,
        source: &OperatorTree,
    ) -> DriverResult<uqa_graph::GraphPostingList> {
        let graph = require_graph_name(source, "MessagePassing.source")?;
        let source_result = self.execute_posting_node(source)?;
        let result = self.with_graph(&graph, |store| {
            uqa_graph::MessagePassing::new(&graph)
                .execute(store)
                .map_err(|error| graph_execution_error("MessagePassing", error))
        })?;
        Ok(uqa_graph::GraphPostingList::from_posting_list(
            &restrict_result_to_source(&result.to_posting_list(), &source_result),
        ))
    }

    pub(super) fn execute_graph_embedding(
        &self,
        source: &OperatorTree,
    ) -> DriverResult<uqa_graph::GraphPostingList> {
        let graph = require_graph_name(source, "GraphEmbedding.source")?;
        let source_result = self.execute_posting_node(source)?;
        let result = self.with_graph(&graph, |store| {
            uqa_graph::GraphEmbedding::new(&graph)
                .execute(store)
                .map_err(|error| graph_execution_error("GraphEmbedding", error))
        })?;
        Ok(uqa_graph::GraphPostingList::from_posting_list(
            &restrict_result_to_source(&result.to_posting_list(), &source_result),
        ))
    }

    pub(super) fn execute_page_rank(
        &self,
        graph: &str,
    ) -> DriverResult<uqa_graph::GraphPostingList> {
        self.with_graph(graph, |store| {
            uqa_graph::PageRank::new(graph)
                .execute(store)
                .map_err(|error| graph_execution_error("PageRank", error))
        })
    }

    pub(super) fn execute_hits(&self, graph: &str) -> DriverResult<uqa_graph::GraphPostingList> {
        self.with_graph(graph, |store| {
            uqa_graph::HITS::new(graph)
                .execute(store)
                .map_err(|error| graph_execution_error("HITS", error))
        })
    }

    pub(super) fn execute_betweenness_centrality(
        &self,
        graph: &str,
    ) -> DriverResult<uqa_graph::GraphPostingList> {
        self.with_graph(graph, |store| {
            uqa_graph::BetweennessCentrality::new(graph)
                .execute(store)
                .map_err(|error| graph_execution_error("BetweennessCentrality", error))
        })
    }

    pub(super) fn execute_temporal_traverse(
        &self,
        start_vertex: u64,
        graph: &str,
        label: Option<&str>,
        max_hops: usize,
        temporal_filter: Option<&uqa_operators::TemporalFilterIR>,
    ) -> DriverResult<uqa_graph::GraphPostingList> {
        let max_hops = u32::try_from(max_hops).map_err(|_| {
            SQLError::TypeMismatch(format!(
                "TemporalTraverse.max_hops is too large: {max_hops}"
            ))
        })?;
        let filter = temporal_filter_from_ir(temporal_filter)?;
        self.with_graph(graph, |store| {
            let mut op = uqa_graph::TemporalTraverse::new(start_vertex, graph)
                .max_hops(max_hops)
                .filter(filter);
            if let Some(label) = label {
                op = op.label(label);
            }
            op.execute(store)
                .map_err(|error| graph_execution_error("TemporalTraverse", error))
        })
    }

    pub(super) fn execute_temporal_pattern_match(
        &self,
        pattern: &uqa_operators::GraphPatternIR,
        graph: &str,
        temporal_filter: Option<&uqa_operators::TemporalFilterIR>,
    ) -> DriverResult<uqa_graph::GraphPostingList> {
        let pattern = graph_pattern_from_ir(pattern);
        let filter = temporal_filter_from_ir(temporal_filter)?;
        self.with_graph(graph, |store| {
            uqa_graph::TemporalPatternMatch::new(pattern, graph)
                .filter(filter)
                .execute(store)
                .map_err(|error| graph_execution_error("TemporalPatternMatch", error))
        })
    }

    pub(super) fn with_graph<R>(
        &self,
        graph: &str,
        execute: impl FnOnce(&uqa_graph::MemoryGraphStore) -> DriverResult<R>,
    ) -> DriverResult<R> {
        self.engine
            .graph_with(graph, execute)
            .map_err(|err| SQLError::Internal(format!("read graph catalog: {err}")))?
            .ok_or_else(|| SQLError::Unsupported(format!("unknown graph {graph:?}")))?
    }
}
