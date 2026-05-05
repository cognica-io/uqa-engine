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
use crate::store::GraphStore;

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
    /// Initial feature: `Some(name)` reads `vertex.properties[name]`
    /// (numeric) and falls back to 0.0 otherwise. `None` initializes
    /// every vertex to 1.0.
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

    pub fn execute<G: GraphStore>(&self, store: &G) -> GraphPostingList {
        let vertices: Vec<VertexId> = store.vertex_ids_in_graph(self.graph).into_iter().collect();
        if vertices.is_empty() {
            return GraphPostingList::new();
        }

        let mut features: BTreeMap<VertexId, f64> = BTreeMap::new();
        for vid in &vertices {
            let value = match (&self.property_name, store.get_vertex(*vid)) {
                (Some(key), Some(vertex)) => match vertex.properties.get(key) {
                    Some(Value::Int(n)) => *n as f64,
                    Some(Value::Float(f)) => *f,
                    Some(Value::Bool(b)) => {
                        if *b {
                            1.0
                        } else {
                            0.0
                        }
                    }
                    _ => 0.0,
                },
                _ => 1.0,
            };
            features.insert(*vid, value);
        }

        for _ in 0..self.k_layers {
            let mut next: BTreeMap<VertexId, f64> = BTreeMap::new();
            for vid in &vertices {
                let mut neighbor_values: Vec<f64> = Vec::new();
                for eid in store.out_edge_ids(*vid, self.graph) {
                    if let Some(edge) = store.get_edge(eid) {
                        neighbor_values.push(*features.get(&edge.target_id).unwrap_or(&0.0));
                    }
                }
                for eid in store.in_edge_ids(*vid, self.graph) {
                    if let Some(edge) = store.get_edge(eid) {
                        neighbor_values.push(*features.get(&edge.source_id).unwrap_or(&0.0));
                    }
                }
                let combined = if neighbor_values.is_empty() {
                    features[vid]
                } else {
                    let agg = match self.aggregation {
                        AggregationKind::Mean => {
                            neighbor_values.iter().sum::<f64>() / neighbor_values.len() as f64
                        }
                        AggregationKind::Sum => neighbor_values.iter().sum::<f64>(),
                        AggregationKind::Max => neighbor_values
                            .iter()
                            .copied()
                            .fold(f64::NEG_INFINITY, f64::max),
                    };
                    features[vid] + agg
                };
                next.insert(*vid, combined);
            }
            features = next;
        }

        let mut sorted = vertices;
        sorted.sort_unstable();
        let mut entries = Vec::with_capacity(sorted.len());
        let mut graph_payloads: BTreeMap<DocId, GraphPayload> = BTreeMap::new();
        for vid in &sorted {
            let calibrated = sigmoid(features[vid]);
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
        GraphPostingList::from_parts(PostingList::from_sorted_unchecked(entries), graph_payloads)
    }
}

fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}
