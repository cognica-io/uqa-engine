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

use std::sync::Arc;

use std::collections::BTreeMap;

use uqa_core::{DocId, Payload, PostingEntry, PostingList, Value, VertexId};

use crate::adjacency::endpoint_index;
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
        if self.k_layers > 0 {
            let mut adjacency = Vec::with_capacity(vertices.len());
            let mut next = Vec::with_capacity(vertices.len());
            // Validate and evaluate the first round in vertex order so a later
            // missing edge cannot overtake an earlier arithmetic error.
            for (index, &id) in vertices.iter().enumerate() {
                let row = self.neighbor_indices(store, &vertices, id)?;
                next.push(self.propagate_vertex(&features, index, &row, id)?);
                adjacency.push(row);
            }
            std::mem::swap(&mut features, &mut next);
            for _ in 1..self.k_layers {
                for (index, &id) in vertices.iter().enumerate() {
                    next[index] = self.propagate_vertex(&features, index, &adjacency[index], id)?;
                }
                std::mem::swap(&mut features, &mut next);
            }
        }
        self.build_result(&vertices, &features)
    }

    fn initial_features<G: GraphStore>(
        &self,
        store: &G,
        vertices: &[VertexId],
    ) -> GraphStoreResult<Vec<f64>> {
        let mut features = Vec::with_capacity(vertices.len());
        for vid in vertices {
            let vertex = store.get_vertex(*vid)?.ok_or_else(|| {
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
            features.push(value);
        }
        Ok(features)
    }

    fn neighbor_indices<G: GraphStore>(
        &self,
        store: &G,
        vertices: &[VertexId],
        id: VertexId,
    ) -> GraphStoreResult<Vec<usize>> {
        let outgoing = store.out_edge_ids(id, self.graph)?;
        let incoming = store.in_edge_ids(id, self.graph)?;
        let count = outgoing.len().checked_add(incoming.len()).ok_or_else(|| {
            GraphStoreError::InvalidQuery(format!(
                "message-passing neighbor count overflows usize for vertex {id}"
            ))
        })?;
        let mut indices = Vec::new();
        indices.try_reserve_exact(count).map_err(|error| {
            GraphStoreError::InvalidQuery(format!(
                "cannot allocate {count} message-passing neighbors for vertex {id}: {error}"
            ))
        })?;
        for eid in outgoing {
            indices.push(endpoint_index(
                store,
                self.graph,
                vertices,
                "message-passing",
                eid,
                |edge| edge.target_id,
            )?);
        }
        for eid in incoming {
            indices.push(endpoint_index(
                store,
                self.graph,
                vertices,
                "message-passing",
                eid,
                |edge| edge.source_id,
            )?);
        }
        Ok(indices)
    }

    fn propagate_vertex(
        &self,
        features: &[f64],
        index: usize,
        neighbors: &[usize],
        id: VertexId,
    ) -> GraphStoreResult<f64> {
        let own = features[index];
        if neighbors.is_empty() {
            return Ok(own);
        }
        let values = neighbors.iter().map(|&other| features[other]);
        let aggregate = aggregate_features(values, self.aggregation, id)?;
        finite_add(own, aggregate, id)
    }

    fn build_result(
        &self,
        vertices: &[VertexId],
        features: &[f64],
    ) -> GraphStoreResult<GraphPostingList> {
        let mut entries = Vec::new();
        entries.try_reserve_exact(vertices.len()).map_err(|error| {
            GraphStoreError::InvalidQuery(format!(
                "cannot allocate {} message-passing result entries: {error}",
                vertices.len()
            ))
        })?;
        let graph_name: Arc<str> = self.graph.into();
        let mut graph_payloads: BTreeMap<DocId, GraphPayload> = BTreeMap::new();
        for (vid, &feature) in vertices.iter().zip(features) {
            let calibrated = sigmoid(feature);
            entries.push(PostingEntry::new(*vid, Payload::with_score(calibrated)));
            graph_payloads.insert(
                *vid,
                GraphPayload {
                    subgraph_vertices: [*vid].into(),
                    subgraph_edges: Arc::default(),
                    graph_name: Arc::clone(&graph_name),
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
    values: impl ExactSizeIterator<Item = f64>,
    aggregation: AggregationKind,
    vertex_id: VertexId,
) -> GraphStoreResult<f64> {
    match aggregation {
        AggregationKind::Max => values.reduce(f64::max).ok_or_else(|| {
            GraphStoreError::CorruptGraph(format!(
                "message-passing aggregation for vertex {vertex_id} has no values"
            ))
        }),
        AggregationKind::Sum | AggregationKind::Mean => {
            let count = values.len();
            let mut total = 0.0;
            for value in values {
                total = finite_add(total, value, vertex_id)?;
            }
            if aggregation == AggregationKind::Mean {
                let count = u64::try_from(count).map_err(|_| {
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
