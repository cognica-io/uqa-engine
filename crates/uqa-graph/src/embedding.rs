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
use crate::store::GraphStore;

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

    pub fn execute<G: GraphStore>(&self, store: &G) -> GraphPostingList {
        let mut vertices: Vec<VertexId> =
            store.vertex_ids_in_graph(self.graph).into_iter().collect();
        vertices.sort_unstable();
        if vertices.is_empty() {
            return GraphPostingList::new();
        }

        // Collect alphabet for label-distribution one-hot.
        let mut all_labels: BTreeSet<String> = BTreeSet::new();
        for vid in &vertices {
            for eid in store.out_edge_ids(*vid, self.graph) {
                if let Some(edge) = store.get_edge(eid) {
                    all_labels.insert(edge.label.clone());
                }
            }
        }
        let label_to_idx: BTreeMap<String, usize> = all_labels
            .iter()
            .enumerate()
            .map(|(i, label)| (label.clone(), i))
            .collect();
        let n_labels = all_labels.len();

        let mut entries: Vec<PostingEntry> = Vec::with_capacity(vertices.len());
        let mut graph_payloads: BTreeMap<DocId, GraphPayload> = BTreeMap::new();
        for vid in &vertices {
            let embedding = self.compute_embedding(store, *vid, &label_to_idx, n_labels);
            let mut fields: BTreeMap<String, Value> = BTreeMap::new();
            fields.insert(
                "_embedding".into(),
                Value::List(embedding.iter().map(|x| Value::Float(*x)).collect()),
            );
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
        GraphPostingList::from_parts(PostingList::from_sorted_unchecked(entries), graph_payloads)
    }

    fn compute_embedding<G: GraphStore>(
        &self,
        store: &G,
        vid: VertexId,
        label_to_idx: &BTreeMap<String, usize>,
        n_labels: usize,
    ) -> Vec<f64> {
        let out_edges: BTreeSet<EdgeId> = store.out_edge_ids(vid, self.graph);
        let in_edges: BTreeSet<EdgeId> = store.in_edge_ids(vid, self.graph);
        let out_degree = out_edges.len();
        let in_degree = in_edges.len();

        let label_dims = n_labels.min(self.dimensions / 2);
        let mut label_dist = vec![0.0f64; label_dims];
        for eid in &out_edges {
            if let Some(edge) = store.get_edge(*eid) {
                if let Some(&idx) = label_to_idx.get(&edge.label) {
                    if idx < label_dims {
                        label_dist[idx] += 1.0;
                    }
                }
            }
        }
        let total: f64 = label_dist.iter().sum();
        if total > 0.0 {
            for v in &mut label_dist {
                *v /= total;
            }
        }

        let mut hop_counts: Vec<f64> = Vec::with_capacity(self.k_layers as usize);
        let mut visited: BTreeSet<VertexId> = BTreeSet::from([vid]);
        let mut frontier: BTreeSet<VertexId> = BTreeSet::from([vid]);
        for _ in 0..self.k_layers {
            let mut next_frontier: BTreeSet<VertexId> = BTreeSet::new();
            for v in &frontier {
                for eid in store.out_edge_ids(*v, self.graph) {
                    if let Some(edge) = store.get_edge(eid) {
                        if !visited.contains(&edge.target_id) {
                            next_frontier.insert(edge.target_id);
                            visited.insert(edge.target_id);
                        }
                    }
                }
            }
            hop_counts.push(next_frontier.len() as f64);
            frontier = next_frontier;
        }

        let mut raw: Vec<f64> = Vec::with_capacity(self.dimensions);
        raw.push(out_degree as f64);
        raw.push(in_degree as f64);
        raw.extend(label_dist);
        raw.extend(hop_counts);
        if raw.len() < self.dimensions {
            raw.resize(self.dimensions, 0.0);
        } else {
            raw.truncate(self.dimensions);
        }
        let norm: f64 = raw.iter().map(|x| x * x).sum::<f64>().sqrt();
        if norm > 0.0 {
            for v in &mut raw {
                *v /= norm;
            }
        }
        raw
    }
}
