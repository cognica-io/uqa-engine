//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cached subgraph pattern results for repeated `GMatch` queries.

use std::collections::{BTreeMap, BTreeSet};

use uqa_core::VertexId;

use crate::operators::GMatch;
use crate::pattern::GraphPattern;
use crate::store::GraphStore;

#[derive(Debug, Clone, Default)]
pub struct SubgraphIndex {
    pattern_to_matches: BTreeMap<String, BTreeSet<BTreeSet<VertexId>>>,
}

impl SubgraphIndex {
    pub fn build<G: GraphStore>(store: &G, patterns: &[GraphPattern], graph: &str) -> Self {
        let mut index = Self::default();
        for pattern in patterns {
            let key = canonicalize(pattern);
            let result = GMatch::new(pattern.clone(), graph).execute(store);
            let mut matches = BTreeSet::new();
            for entry in result.inner().entries() {
                if let Some(payload) = result.get_graph_payload(entry.doc_id) {
                    matches.insert(payload.subgraph_vertices.iter().copied().collect());
                }
            }
            index.pattern_to_matches.insert(key, matches);
        }
        index
    }

    pub fn lookup(&self, pattern: &GraphPattern) -> Option<&BTreeSet<BTreeSet<VertexId>>> {
        self.pattern_to_matches.get(&canonicalize(pattern))
    }

    pub fn has_pattern(&self, pattern: &GraphPattern) -> bool {
        self.pattern_to_matches.contains_key(&canonicalize(pattern))
    }

    pub fn indexed_patterns(&self) -> Vec<String> {
        self.pattern_to_matches.keys().cloned().collect()
    }

    pub fn invalidate_by_edge_labels(&mut self, labels: &BTreeSet<String>) {
        self.pattern_to_matches
            .retain(|key, _| !labels.iter().any(|label| key.contains(label)));
    }
}

fn canonicalize(pattern: &GraphPattern) -> String {
    let mut vertices: Vec<String> = pattern
        .vertex_patterns
        .iter()
        .map(|vp| format!("{}:{:?}", vp.variable, vp.constraints))
        .collect();
    vertices.sort();

    let mut edges: Vec<String> = pattern
        .edge_patterns
        .iter()
        .map(|ep| {
            format!(
                "{}>{}:{:?}:{:?}:{}",
                ep.source_var, ep.target_var, ep.label, ep.constraints, ep.negated
            )
        })
        .collect();
    edges.sort();

    format!("V:{}|E:{}", vertices.join(","), edges.join(","))
}

#[cfg(test)]
mod tests {
    use uqa_core::{Edge, Vertex};

    use super::*;
    use crate::{EdgePattern, GraphStore, MemoryGraphStore, VertexPattern};

    #[test]
    fn build_and_lookup_cached_pattern() {
        let graph = "g";
        let mut store = MemoryGraphStore::new();
        store.create_graph(graph);
        store.add_vertex(Vertex::new(1, "Person"), graph);
        store.add_vertex(Vertex::new(2, "Person"), graph);
        store.add_edge(Edge::new(10, 1, 2, "knows"), graph);

        let pattern = GraphPattern::new()
            .add_vertex(VertexPattern::new("a"))
            .add_vertex(VertexPattern::new("b"))
            .add_edge(EdgePattern::new("a", "b").with_label("knows"));
        let index = SubgraphIndex::build(&store, std::slice::from_ref(&pattern), graph);

        assert!(index.has_pattern(&pattern));
        assert_eq!(index.lookup(&pattern).unwrap().len(), 1);
    }
}
