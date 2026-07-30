//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Structural per-vertex graph embeddings.
//!
//! For each vertex we assemble a feature vector from
//!
//! * out-degree, in-degree
//! * out-edge label distribution (capped at `dims / 2`)
//! * k-hop frontier counts (one per layer)
//!
//! padded or truncated to `dims` and L2-normalized. The result lives
//! on the payload's `_embedding` field as a `Value::List(Float)` so
//! downstream vector-similarity scorers can consume it directly.

use std::collections::{BTreeMap, BTreeSet};

use uqa_core::{DocId, EdgeId, Payload, PostingEntry, PostingList, Value, VertexId};

use crate::posting_list::{GraphPayload, GraphPostingList};
use crate::store::{GraphStore, GraphStoreError, GraphStoreResult};

pub const MAX_GRAPH_EMBEDDING_DIMENSIONS: usize = 4_096;
pub const MAX_GRAPH_EMBEDDING_LAYERS: u32 = 256;
const MAX_EXACT_F64_INTEGER: u64 = 9_007_199_254_740_992;

pub struct GraphEmbedding<'a> {
    pub graph: &'a str,
    pub dimensions: usize,
    pub k_layers: u32,
}

impl<'a> GraphEmbedding<'a> {
    pub fn new(graph: &'a str) -> Self {
        Self {
            graph,
            dimensions: 32,
            k_layers: 2,
        }
    }

    pub fn dimensions(mut self, dims: usize) -> Self {
        self.dimensions = dims;
        self
    }

    pub fn k_layers(mut self, k: u32) -> Self {
        self.k_layers = k;
        self
    }

    pub fn execute<G: GraphStore>(&self, store: &G) -> GraphStoreResult<GraphPostingList> {
        if self.dimensions == 0 || self.dimensions > MAX_GRAPH_EMBEDDING_DIMENSIONS {
            return Err(GraphStoreError::InvalidQuery(format!(
                "graph embedding dimensions must be in 1..={MAX_GRAPH_EMBEDDING_DIMENSIONS}, got {}",
                self.dimensions
            )));
        }
        if self.k_layers > MAX_GRAPH_EMBEDDING_LAYERS {
            return Err(GraphStoreError::InvalidQuery(format!(
                "graph embedding layer count {} exceeds limit {MAX_GRAPH_EMBEDDING_LAYERS}",
                self.k_layers
            )));
        }
        let vertex_ids = store.vertex_ids_in_graph(self.graph)?;
        let mut vertices: Vec<VertexId> = Vec::new();
        vertices
            .try_reserve_exact(vertex_ids.len())
            .map_err(|error| allocation_error("vertex id list", vertex_ids.len(), &error))?;
        vertices.extend(vertex_ids.iter().copied());
        if vertices.is_empty() {
            return Ok(GraphPostingList::new());
        }
        for vid in &vertices {
            if store.get_vertex(*vid).is_none() {
                return Err(GraphStoreError::CorruptGraph(format!(
                    "graph embedding graph {:?} references missing vertex {vid}",
                    self.graph
                )));
            }
        }

        // Collect alphabet for label-distribution one-hot.
        let mut all_labels: BTreeSet<String> = BTreeSet::new();
        for vid in &vertices {
            for eid in store.out_edge_ids(*vid, self.graph)? {
                let edge = store.get_edge(eid).ok_or_else(|| {
                    GraphStoreError::CorruptGraph(format!("missing embedding edge {eid}"))
                })?;
                all_labels.insert(edge.label.clone());
            }
        }
        let label_to_idx: BTreeMap<String, usize> = all_labels
            .iter()
            .enumerate()
            .map(|(i, label)| (label.clone(), i))
            .collect();
        let n_labels = all_labels.len();

        let mut entries: Vec<PostingEntry> = Vec::new();
        entries
            .try_reserve_exact(vertices.len())
            .map_err(|error| allocation_error("posting entries", vertices.len(), &error))?;
        let mut graph_payloads: BTreeMap<DocId, GraphPayload> = BTreeMap::new();
        for vid in &vertices {
            let embedding =
                self.compute_embedding(store, *vid, &vertex_ids, &label_to_idx, n_labels)?;
            let mut fields: BTreeMap<String, Value> = BTreeMap::new();
            let mut embedding_values = Vec::new();
            embedding_values
                .try_reserve_exact(embedding.len())
                .map_err(|error| allocation_error("embedding payload", embedding.len(), &error))?;
            embedding_values.extend(embedding.iter().map(|x| Value::Float(*x)));
            fields.insert("_embedding".into(), Value::List(embedding_values));
            let payload = Payload {
                positions: Vec::new(),
                score: 0.0,
                fields,
            };
            entries.push(PostingEntry::new(*vid, payload));
            graph_payloads.insert(
                *vid,
                GraphPayload {
                    subgraph_vertices: vec![*vid],
                    subgraph_edges: Vec::new(),
                    graph_name: self.graph.to_string(),
                    score_override: None,
                },
            );
        }
        Ok(GraphPostingList::from_parts(
            PostingList::from_sorted_unchecked(entries),
            graph_payloads,
        ))
    }

    fn compute_embedding<G: GraphStore>(
        &self,
        store: &G,
        vid: VertexId,
        graph_vertices: &BTreeSet<VertexId>,
        label_to_idx: &BTreeMap<String, usize>,
        n_labels: usize,
    ) -> GraphStoreResult<Vec<f64>> {
        let out_edges: BTreeSet<EdgeId> = store.out_edge_ids(vid, self.graph)?;
        let in_edges: BTreeSet<EdgeId> = store.in_edge_ids(vid, self.graph)?;
        let out_degree = out_edges.len();
        let in_degree = in_edges.len();

        let label_dims = n_labels.min(self.dimensions / 2);
        let mut label_dist = Vec::new();
        label_dist
            .try_reserve_exact(label_dims)
            .map_err(|error| allocation_error("edge-label distribution", label_dims, &error))?;
        label_dist.resize(label_dims, 0.0_f64);
        for eid in &out_edges {
            let edge = store.get_edge(*eid).ok_or_else(|| {
                GraphStoreError::CorruptGraph(format!("missing embedding edge {eid}"))
            })?;
            if let Some(&idx) = label_to_idx.get(&edge.label) {
                if idx < label_dims {
                    label_dist[idx] += 1.0;
                }
            }
        }
        let total: f64 = label_dist.iter().sum();
        if total > 0.0 {
            for v in &mut label_dist {
                *v /= total;
            }
        }

        let layer_capacity = usize::try_from(self.k_layers).map_err(|_| {
            GraphStoreError::InvalidQuery(format!(
                "graph embedding layer count {} does not fit usize",
                self.k_layers
            ))
        })?;
        let mut hop_counts: Vec<f64> = Vec::new();
        hop_counts
            .try_reserve_exact(layer_capacity)
            .map_err(|error| allocation_error("hop counts", layer_capacity, &error))?;
        let mut visited: BTreeSet<VertexId> = BTreeSet::from([vid]);
        let mut frontier: BTreeSet<VertexId> = BTreeSet::from([vid]);
        for _ in 0..self.k_layers {
            let mut next_frontier: BTreeSet<VertexId> = BTreeSet::new();
            for v in &frontier {
                for eid in store.out_edge_ids(*v, self.graph)? {
                    let edge = store.get_edge(eid).ok_or_else(|| {
                        GraphStoreError::CorruptGraph(format!("missing embedding edge {eid}"))
                    })?;
                    if !graph_vertices.contains(&edge.target_id) {
                        return Err(GraphStoreError::CorruptGraph(format!(
                            "embedding edge {eid} targets vertex {} outside graph {:?}",
                            edge.target_id, self.graph
                        )));
                    }
                    if !visited.contains(&edge.target_id) {
                        next_frontier.insert(edge.target_id);
                        visited.insert(edge.target_id);
                    }
                }
            }
            hop_counts.push(usize_to_f64_exact(
                next_frontier.len(),
                "graph embedding hop frontier",
            )?);
            frontier = next_frontier;
        }

        let generated_features = 2_usize
            .checked_add(label_dims)
            .and_then(|value| value.checked_add(layer_capacity))
            .ok_or_else(|| {
                GraphStoreError::InvalidQuery(
                    "graph embedding feature count overflows usize".to_string(),
                )
            })?;
        let raw_capacity = self.dimensions.max(generated_features);
        let mut raw: Vec<f64> = Vec::new();
        raw.try_reserve_exact(raw_capacity)
            .map_err(|error| allocation_error("raw embedding", raw_capacity, &error))?;
        raw.push(usize_to_f64_exact(
            out_degree,
            "graph embedding out-degree",
        )?);
        raw.push(usize_to_f64_exact(in_degree, "graph embedding in-degree")?);
        raw.extend(label_dist);
        raw.extend(hop_counts);
        normalize_embedding(raw, self.dimensions, vid)
    }
}

fn usize_to_f64_exact(value: usize, context: &str) -> GraphStoreResult<f64> {
    if !u64::try_from(value).is_ok_and(|value| value <= MAX_EXACT_F64_INTEGER) {
        return Err(GraphStoreError::InvalidQuery(format!(
            "{context} {value} exceeds the exact f64 integer range"
        )));
    }
    Ok(value as f64)
}

fn allocation_error(
    context: &str,
    elements: usize,
    error: &std::collections::TryReserveError,
) -> GraphStoreError {
    GraphStoreError::InvalidQuery(format!(
        "cannot allocate {elements} elements for graph embedding {context}: {error}"
    ))
}

fn normalize_embedding(
    mut raw: Vec<f64>,
    dimensions: usize,
    vertex_id: VertexId,
) -> GraphStoreResult<Vec<f64>> {
    if raw.len() < dimensions {
        raw.resize(dimensions, 0.0);
    } else {
        raw.truncate(dimensions);
    }
    let norm = raw.iter().map(|value| value * value).sum::<f64>().sqrt();
    if !norm.is_finite() {
        return Err(GraphStoreError::InvalidQuery(format!(
            "graph embedding norm is not finite for vertex {vertex_id}"
        )));
    }
    if norm > 0.0 {
        for value in &mut raw {
            *value /= norm;
        }
    }
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use uqa_core::{Edge, Vertex};

    use super::*;
    use crate::memory_store::MemoryGraphStore;

    #[test]
    fn missing_edge_record_is_corruption_not_an_empty_label_bucket() {
        let mut store = MemoryGraphStore::new();
        store.create_graph("g");
        store.add_vertex(Vertex::new(1, "n"), "g").unwrap();
        store.add_vertex(Vertex::new(2, "n"), "g").unwrap();
        store.add_edge(Edge::new(10, 1, 2, "edge"), "g").unwrap();
        store.remove_edge_record_for_corruption_test(10);

        let error = GraphEmbedding::new("g").execute(&store).unwrap_err();
        assert!(matches!(error, GraphStoreError::CorruptGraph(_)));
    }
}
