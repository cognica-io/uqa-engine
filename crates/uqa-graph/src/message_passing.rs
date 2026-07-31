//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! K-layer message-passing aggregation over graph vertices.
//!
//! Each round each vertex pulls feature values from its neighbors
//! (out-edges and in-edges merged) and combines them with its own
//! prior feature via a residual sum. Final per-vertex feature is
//! squashed through the sigmoid to land in `[0, 1]`.

use std::collections::BTreeMap;

use uqa_core::{DocId, Payload, PostingEntry, PostingList, Value, VertexId};

use crate::posting_list::{GraphPayload, GraphPostingList};
use crate::store::{GraphStore, GraphStoreError, GraphStoreResult};

/// Protects the public graph API from accidentally scheduling billions of
/// full-graph propagation rounds from an unchecked `u32` input.
pub const MAX_MESSAGE_PASSING_LAYERS: u32 = 256;
const MAX_EXACT_F64_INTEGER: u64 = 1_u64 << 53;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggregationKind {
    Mean,
    Sum,
    Max,
}

pub struct MessagePassing<'a> {
    pub graph: &'a str,
    pub k_layers: u32,
    pub aggregation: AggregationKind,
    /// Initial feature: `Some(name)` reads `vertex.properties[name]` and
    /// uses 0.0 only when the property is absent. Present values must be a
    /// finite float, an exactly representable integer, or a boolean. `None`
    /// initializes every vertex to 1.0.
    pub property_name: Option<String>,
}

impl<'a> MessagePassing<'a> {
    pub fn new(graph: &'a str) -> Self {
        Self {
            graph,
            k_layers: 2,
            aggregation: AggregationKind::Mean,
            property_name: None,
        }
    }

    pub fn k_layers(mut self, k: u32) -> Self {
        self.k_layers = k;
        self
    }

    pub fn aggregation(mut self, kind: AggregationKind) -> Self {
        self.aggregation = kind;
        self
    }

    pub fn property_name(mut self, name: impl Into<String>) -> Self {
        self.property_name = Some(name.into());
        self
    }

    pub fn execute<G: GraphStore>(&self, store: &G) -> GraphStoreResult<GraphPostingList> {
        if self.k_layers > MAX_MESSAGE_PASSING_LAYERS {
            return Err(GraphStoreError::InvalidQuery(format!(
                "message-passing layer count {} exceeds limit {MAX_MESSAGE_PASSING_LAYERS}",
                self.k_layers
            )));
        }
        let vertices: Vec<VertexId> = store.vertex_ids_in_graph(self.graph)?.into_iter().collect();
        if vertices.is_empty() {
            return Ok(GraphPostingList::new());
        }

        let mut features = self.initial_features(store, &vertices)?;
        for _ in 0..self.k_layers {
            features = self.propagate_layer(store, &vertices, &features)?;
        }
        self.build_result(vertices, &features)
    }

    fn initial_features<G: GraphStore>(
        &self,
        store: &G,
        vertices: &[VertexId],
    ) -> GraphStoreResult<BTreeMap<VertexId, f64>> {
        let mut features = BTreeMap::new();
        for vid in vertices {
            let vertex = store.get_vertex(*vid).ok_or_else(|| {
                GraphStoreError::CorruptGraph(format!(
                    "message-passing graph {:?} references missing vertex {vid}",
                    self.graph
                ))
            })?;
            let value = match &self.property_name {
                Some(key) => match vertex.properties.get(key) {
                    Some(value) => numeric_feature(value, key, *vid)?,
                    None => 0.0,
                },
                None => 1.0,
            };
            features.insert(*vid, value);
        }
        Ok(features)
    }

    fn propagate_layer<G: GraphStore>(
        &self,
        store: &G,
        vertices: &[VertexId],
        features: &BTreeMap<VertexId, f64>,
    ) -> GraphStoreResult<BTreeMap<VertexId, f64>> {
        let mut next = BTreeMap::new();
        for vid in vertices {
            let out_edges = store.out_edge_ids(*vid, self.graph)?;
            let in_edges = store.in_edge_ids(*vid, self.graph)?;
            let neighbor_count = out_edges.len().checked_add(in_edges.len()).ok_or_else(|| {
                GraphStoreError::InvalidQuery(format!(
                    "message-passing neighbor count overflows usize for vertex {vid}"
                ))
            })?;
            let mut neighbor_values: Vec<f64> = Vec::new();
            neighbor_values
                .try_reserve_exact(neighbor_count)
                .map_err(|error| {
                    GraphStoreError::InvalidQuery(format!(
                        "cannot allocate {neighbor_count} message-passing neighbors for vertex {vid}: {error}"
                    ))
                })?;
            for eid in out_edges {
                let edge = store.get_edge(eid).ok_or_else(|| {
                    GraphStoreError::CorruptGraph(format!("missing message-passing edge {eid}"))
                })?;
                let value = features.get(&edge.target_id).ok_or_else(|| {
                    GraphStoreError::CorruptGraph(format!(
                        "edge {eid} references vertex {} outside graph {:?}",
                        edge.target_id, self.graph
                    ))
                })?;
                neighbor_values.push(*value);
            }
            for eid in in_edges {
                let edge = store.get_edge(eid).ok_or_else(|| {
                    GraphStoreError::CorruptGraph(format!("missing message-passing edge {eid}"))
                })?;
                let value = features.get(&edge.source_id).ok_or_else(|| {
                    GraphStoreError::CorruptGraph(format!(
                        "edge {eid} references vertex {} outside graph {:?}",
                        edge.source_id, self.graph
                    ))
                })?;
                neighbor_values.push(*value);
            }
            let own_feature = features.get(vid).copied().ok_or_else(|| {
                GraphStoreError::CorruptGraph(format!(
                    "message-passing feature state is missing vertex {vid}"
                ))
            })?;
            let combined = if neighbor_values.is_empty() {
                own_feature
            } else {
                let agg = aggregate_features(&neighbor_values, self.aggregation, *vid)?;
                finite_add(own_feature, agg, *vid)?
            };
            next.insert(*vid, combined);
        }
        Ok(next)
    }

    fn build_result(
        &self,
        mut vertices: Vec<VertexId>,
        features: &BTreeMap<VertexId, f64>,
    ) -> GraphStoreResult<GraphPostingList> {
        vertices.sort_unstable();
        let mut entries = Vec::new();
        entries.try_reserve_exact(vertices.len()).map_err(|error| {
            GraphStoreError::InvalidQuery(format!(
                "cannot allocate {} message-passing result entries: {error}",
                vertices.len()
            ))
        })?;
        let mut graph_payloads: BTreeMap<DocId, GraphPayload> = BTreeMap::new();
        for vid in &vertices {
            let feature = features.get(vid).copied().ok_or_else(|| {
                GraphStoreError::CorruptGraph(format!(
                    "message-passing final state is missing vertex {vid}"
                ))
            })?;
            let calibrated = sigmoid(feature);
            entries.push(PostingEntry::new(*vid, Payload::with_score(calibrated)));
            graph_payloads.insert(
                *vid,
                GraphPayload {
                    subgraph_vertices: vec![*vid],
                    subgraph_edges: Vec::new(),
                    graph_name: self.graph.to_string(),
                    score_override: Some(calibrated),
                },
            );
        }
        GraphPostingList::try_from_parts(
            PostingList::from_sorted_unchecked(entries),
            graph_payloads,
        )
        .map_err(Into::into)
    }
}

fn numeric_feature(value: &Value, property: &str, vertex_id: VertexId) -> GraphStoreResult<f64> {
    match value {
        Value::Float(value) if value.is_finite() => Ok(*value),
        Value::Float(value) => Err(GraphStoreError::InvalidQuery(format!(
            "message-passing property {property:?} on vertex {vertex_id} must be finite, got {value}"
        ))),
        Value::Int(value) if value.unsigned_abs() <= MAX_EXACT_F64_INTEGER => Ok(*value as f64),
        Value::Int(value) => Err(GraphStoreError::InvalidQuery(format!(
            "message-passing property {property:?} integer {value} on vertex {vertex_id} cannot be represented exactly as f64"
        ))),
        Value::Bool(value) => Ok(if *value { 1.0 } else { 0.0 }),
        other => Err(GraphStoreError::InvalidQuery(format!(
            "message-passing property {property:?} on vertex {vertex_id} must be numeric or boolean, got {other:?}"
        ))),
    }
}

fn aggregate_features(
    values: &[f64],
    aggregation: AggregationKind,
    vertex_id: VertexId,
) -> GraphStoreResult<f64> {
    match aggregation {
        AggregationKind::Max => values.iter().copied().reduce(f64::max).ok_or_else(|| {
            GraphStoreError::CorruptGraph(format!(
                "message-passing aggregation for vertex {vertex_id} has no values"
            ))
        }),
        AggregationKind::Sum | AggregationKind::Mean => {
            let mut total = 0.0;
            for value in values {
                total = finite_add(total, *value, vertex_id)?;
            }
            if aggregation == AggregationKind::Mean {
                let count = u64::try_from(values.len()).map_err(|_| {
                    GraphStoreError::InvalidQuery(format!(
                        "message-passing neighbor count exceeds u64 for vertex {vertex_id}"
                    ))
                })?;
                if count > MAX_EXACT_F64_INTEGER {
                    return Err(GraphStoreError::InvalidQuery(format!(
                        "message-passing neighbor count {count} for vertex {vertex_id} cannot be represented exactly as f64"
                    )));
                }
                total /= count as f64;
            }
            Ok(total)
        }
    }
}

fn finite_add(left: f64, right: f64, vertex_id: VertexId) -> GraphStoreResult<f64> {
    let result = left + right;
    if !result.is_finite() {
        return Err(GraphStoreError::InvalidQuery(format!(
            "message-passing feature accumulation overflowed for vertex {vertex_id}"
        )));
    }
    Ok(result)
}

fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}
