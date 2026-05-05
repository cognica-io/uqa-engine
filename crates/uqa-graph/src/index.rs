//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Specialized graph indexes for accelerating traversal and RPQ
//! evaluation (Section 6.4, Paper 2).
//!
//! `LabelIndex` exposes label cardinality and label-to-vertex sets on
//! top of what the underlying [`GraphStore`] already tracks.
//! `PathIndex` pre-computes the `(start, end)` reachability set for a
//! list of label sequences so the RPQ operator can short-circuit when
//! the input expression is a pure label-concatenation.

use std::collections::{BTreeMap, BTreeSet};

use uqa_core::{EdgeId, VertexId};

use crate::store::GraphStore;

#[derive(Debug, Clone, Default)]
pub struct LabelIndex {
    label_to_edges: BTreeMap<String, Vec<EdgeId>>,
    label_to_vertices: BTreeMap<String, BTreeSet<VertexId>>,
}

impl LabelIndex {
    pub fn build<G: GraphStore>(store: &G, graph: &str) -> Self {
        let mut idx = Self::default();
        for vid in store.vertex_ids_in_graph(graph) {
            for eid in store.out_edge_ids(vid, graph) {
                let Some(edge) = store.get_edge(eid) else {
                    continue;
                };
                idx.label_to_edges
                    .entry(edge.label.clone())
                    .or_default()
                    .push(eid);
                let vset = idx.label_to_vertices.entry(edge.label.clone()).or_default();
                vset.insert(edge.source_id);
                vset.insert(edge.target_id);
            }
        }
        for edges in idx.label_to_edges.values_mut() {
            edges.sort_unstable();
            edges.dedup();
        }
        idx
    }

    pub fn edges_by_label(&self, label: &str) -> &[EdgeId] {
        self.label_to_edges.get(label).map_or(&[], Vec::as_slice)
    }

    pub fn vertices_by_label(&self, label: &str) -> Option<&BTreeSet<VertexId>> {
        self.label_to_vertices.get(label)
    }

    pub fn labels(&self) -> Vec<String> {
        self.label_to_edges.keys().cloned().collect()
    }

    pub fn label_count(&self, label: &str) -> usize {
        self.label_to_edges.get(label).map_or(0, Vec::len)
    }
}

/// Pre-indexed reachable `(start, end)` pairs for fixed label
/// sequences. Lookup is keyed by the slash-joined sequence so the RPQ
/// operator can lift a `Label / Label / ...` expression into a direct
/// hit without running NFA simulation.
#[derive(Debug, Clone, Default)]
pub struct PathIndex {
    path_pairs: BTreeMap<String, BTreeSet<(VertexId, VertexId)>>,
}

impl PathIndex {
    pub fn build<G: GraphStore>(store: &G, graph: &str, label_sequences: &[Vec<String>]) -> Self {
        let mut idx = Self::default();
        for seq in label_sequences {
            let key = seq.join("/");
            let mut pairs: BTreeSet<(VertexId, VertexId)> = BTreeSet::new();
            for start in store.vertex_ids_in_graph(graph) {
                let ends = follow_path(store, graph, start, seq);
                for end in ends {
                    pairs.insert((start, end));
                }
            }
            idx.path_pairs.insert(key, pairs);
        }
        idx
    }

    pub fn lookup(&self, label_sequence: &[String]) -> Option<&BTreeSet<(VertexId, VertexId)>> {
        let key = label_sequence.join("/");
        self.path_pairs.get(&key)
    }

    pub fn has_path(&self, label_sequence: &[String]) -> bool {
        let key = label_sequence.join("/");
        self.path_pairs.contains_key(&key)
    }

    pub fn indexed_paths(&self) -> Vec<String> {
        self.path_pairs.keys().cloned().collect()
    }
}

fn follow_path<G: GraphStore>(
    store: &G,
    graph: &str,
    start: VertexId,
    labels: &[String],
) -> BTreeSet<VertexId> {
    let mut current: BTreeSet<VertexId> = BTreeSet::from([start]);
    for label in labels {
        let mut next_set: BTreeSet<VertexId> = BTreeSet::new();
        for vid in &current {
            for eid in store.out_edge_ids(*vid, graph) {
                if let Some(edge) = store.get_edge(eid) {
                    if &edge.label == label {
                        next_set.insert(edge.target_id);
                    }
                }
            }
        }
        current = next_set;
        if current.is_empty() {
            break;
        }
    }
    current
}
