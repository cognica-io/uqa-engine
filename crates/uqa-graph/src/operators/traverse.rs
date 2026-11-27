//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded breadth-first graph traversal.

use super::{
    BTreeSet, EdgeId, GraphPostingList, GraphStore, GraphStoreError, GraphStoreResult, VertexId,
    VertexPredicate, DEFAULT_GRAPH_SCORE,
};

/// `Traverse_{v,l,k}` (Definition 2.2.1): BFS from `start_vertex` along
/// edges with `label` (any label when `None`) up to `max_hops` hops.
/// Each visited vertex becomes its own entry in the result.
pub struct Traverse<'a> {
    pub start_vertex: VertexId,
    pub graph: &'a str,
    pub label: Option<&'a str>,
    pub max_hops: u32,
    pub vertex_predicate: Option<VertexPredicate>,
    pub score: f64,
}

impl<'a> Traverse<'a> {
    pub fn new(start: VertexId, graph: &'a str) -> Self {
        Self {
            start_vertex: start,
            graph,
            label: None,
            max_hops: 1,
            vertex_predicate: None,
            score: DEFAULT_GRAPH_SCORE,
        }
    }

    pub fn label(mut self, label: &'a str) -> Self {
        self.label = Some(label);
        self
    }

    pub fn max_hops(mut self, hops: u32) -> Self {
        self.max_hops = hops;
        self
    }

    pub fn predicate(mut self, p: VertexPredicate) -> Self {
        self.vertex_predicate = Some(p);
        self
    }

    pub fn execute<G: GraphStore>(&self, store: &G) -> GraphStoreResult<GraphPostingList> {
        store.require_vertex_in_graph(self.start_vertex, self.graph)?;
        let mut visited: BTreeSet<VertexId> = BTreeSet::new();
        let mut frontier: BTreeSet<VertexId> = BTreeSet::new();
        frontier.insert(self.start_vertex);
        let mut all_edges: BTreeSet<EdgeId> = BTreeSet::new();

        for _ in 0..self.max_hops {
            let mut next_frontier: BTreeSet<VertexId> = BTreeSet::new();
            for v in &frontier {
                for eid in store.out_edge_ids(*v, self.graph)? {
                    let edge = store.get_edge(eid)?.ok_or_else(|| {
                        GraphStoreError::CorruptGraph(format!("missing traversal edge {eid}"))
                    })?;
                    if let Some(want) = self.label {
                        if edge.label != want {
                            continue;
                        }
                    }
                    let neighbor = edge.target_id;
                    if visited.contains(&neighbor) || frontier.contains(&neighbor) {
                        // Already explored or about to be — but still record the edge.
                        all_edges.insert(eid);
                        continue;
                    }
                    if let Some(pred) = &self.vertex_predicate {
                        let vtx = store.get_vertex(neighbor)?.ok_or_else(|| {
                            GraphStoreError::CorruptGraph(format!(
                                "traversal edge {eid} references missing vertex {neighbor}"
                            ))
                        })?;
                        if !pred.matches(&vtx) {
                            continue;
                        }
                    }
                    next_frontier.insert(neighbor);
                    all_edges.insert(eid);
                }
            }
            visited.extend(frontier);
            frontier = next_frontier;
            if frontier.is_empty() {
                break;
            }
        }
        visited.append(&mut frontier);

        Ok(GraphPostingList::from_traversal(
            visited, all_edges, self.graph, self.score,
        ))
    }
}

// -------------------------------------------------------------------------
// VertexMatch
// -------------------------------------------------------------------------
