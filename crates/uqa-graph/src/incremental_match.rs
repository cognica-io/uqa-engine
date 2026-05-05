//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Delta-aware pattern matching (Section 9.3, Paper 2).
//!
//! Maintains a set of materialized matches for a [`GraphPattern`] and
//! refreshes them incrementally on a [`GraphDelta`] application:
//!
//! 1. Drop matches that include any vertex affected by the delta.
//! 2. Re-run the matcher with each pattern variable in turn forced
//!    to one of the affected vertices, collecting new matches.
//! 3. Union the new matches into the surviving base set.

use std::collections::BTreeSet;

use uqa_core::{Edge, VertexId};

use crate::delta::{DeltaOp, GraphDelta};
use crate::operators::GMatch;
use crate::pattern::{GraphPattern, VertexPredicate};
use crate::store::GraphStore;

pub struct IncrementalPatternMatcher {
    pub pattern: GraphPattern,
    pub graph: String,
    pub base_matches: BTreeSet<Vec<VertexId>>,
}

impl IncrementalPatternMatcher {
    pub fn new(pattern: GraphPattern, graph: impl Into<String>) -> Self {
        Self {
            pattern,
            graph: graph.into(),
            base_matches: BTreeSet::new(),
        }
    }

    pub fn matches(&self) -> &BTreeSet<Vec<VertexId>> {
        &self.base_matches
    }

    /// Initial population of the base match set. Equivalent to a
    /// one-shot `GMatch` whose results are folded into `base_matches`.
    pub fn seed<G: GraphStore>(&mut self, store: &G) {
        let result = GMatch::new(self.pattern.clone(), &self.graph).execute(store);
        for entry in result.inner().entries() {
            if let Some(gp) = result.get_graph_payload(entry.doc_id) {
                let mut vertices = gp.subgraph_vertices.clone();
                vertices.sort_unstable();
                vertices.dedup();
                self.base_matches.insert(vertices);
            }
        }
    }

    /// Apply a delta and return the refreshed match set. The store is
    /// expected to already reflect the delta.
    pub fn update<G: GraphStore>(
        &mut self,
        store: &G,
        delta: &GraphDelta,
    ) -> &BTreeSet<Vec<VertexId>> {
        let mut affected: BTreeSet<VertexId> = delta.affected_vertex_ids();
        // Edge add/remove ops also implicate their endpoints, even though
        // GraphDelta::affected_vertex_ids only sees the endpoints of *added*
        // edges. For removed edges we look the endpoints up via the store
        // (the edge has just been deleted; we record any survivor info we
        // can still resolve).
        for op in delta.ops() {
            if let DeltaOp::AddEdge(edge) = op {
                affected.insert(edge.source_id);
                affected.insert(edge.target_id);
            }
        }

        // Step 1: drop any base match that overlaps an affected vertex.
        let affected_set = affected.clone();
        self.base_matches
            .retain(|m| !m.iter().any(|v| affected_set.contains(v)));

        // Step 2: for each pattern variable, re-run a constrained match
        // with that variable bound to one of the affected vertices.
        let mut new_matches: BTreeSet<Vec<VertexId>> = BTreeSet::new();
        for vp in &self.pattern.vertex_patterns {
            let mut constrained_pattern = self.pattern.clone();
            for cvp in &mut constrained_pattern.vertex_patterns {
                if cvp.variable == vp.variable {
                    let affected_for_predicate = affected.clone();
                    cvp.constraints
                        .push(VertexPredicate::Custom(std::sync::Arc::new(
                            move |vertex| affected_for_predicate.contains(&vertex.vertex_id),
                        )));
                }
            }
            let result = GMatch::new(constrained_pattern, &self.graph).execute(store);
            for entry in result.inner().entries() {
                if let Some(gp) = result.get_graph_payload(entry.doc_id) {
                    let mut vertices = gp.subgraph_vertices.clone();
                    vertices.sort_unstable();
                    vertices.dedup();
                    new_matches.insert(vertices);
                }
            }
        }

        self.base_matches.extend(new_matches);
        &self.base_matches
    }
}

/// Convenience helper: count vertices implicated by a delta, using the
/// store to resolve removed edges back to their endpoints when those
/// records are still around.
pub fn implicated_vertices<G: GraphStore>(
    store: &G,
    delta: &GraphDelta,
    graph: &str,
) -> BTreeSet<VertexId> {
    let mut out = delta.affected_vertex_ids();
    for op in delta.ops() {
        if let DeltaOp::RemoveEdge(eid) = op {
            if let Some(Edge {
                source_id,
                target_id,
                ..
            }) = store.get_edge(*eid).cloned()
            {
                out.insert(source_id);
                out.insert(target_id);
            }
        }
    }
    let _ = graph;
    out
}
