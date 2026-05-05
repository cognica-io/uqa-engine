//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Temporal filtering and traversal (Section 10, Paper 2).
//!
//! Edges may carry `valid_from` / `valid_to` properties (numeric
//! seconds-since-epoch by convention). [`TemporalFilter`] accepts an
//! edge whose validity interval covers a query timestamp or overlaps a
//! query range. [`TemporalTraverse`] is `Traverse` with the filter
//! applied to each edge before it is followed.

use std::collections::{BTreeMap, BTreeSet};

use uqa_core::{DocId, EdgeId, Payload, PostingEntry, PostingList, Value, VertexId};

use crate::operators::DEFAULT_GRAPH_SCORE;
use crate::posting_list::{GraphPayload, GraphPostingList};
use crate::store::GraphStore;

/// Time-aware edge filter. `Timestamp(t)` accepts an edge if
/// `valid_from <= t <= valid_to`; `Range(a, b)` accepts an edge whose
/// validity interval overlaps `[a, b]`. An edge with neither
/// `valid_from` nor `valid_to` is always accepted.
#[derive(Debug, Clone, Copy)]
pub enum TemporalFilter {
    /// Accept everything.
    Any,
    /// Accept edges valid at exactly this timestamp.
    Timestamp(f64),
    /// Accept edges whose validity interval overlaps the closed range.
    Range(f64, f64),
}

impl TemporalFilter {
    pub fn is_valid(&self, properties: &BTreeMap<String, Value>) -> bool {
        let valid_from = numeric(properties.get("valid_from"));
        let valid_to = numeric(properties.get("valid_to"));
        if valid_from.is_none() && valid_to.is_none() {
            return true;
        }
        let vf = valid_from.unwrap_or(f64::NEG_INFINITY);
        let vt = valid_to.unwrap_or(f64::INFINITY);
        match *self {
            TemporalFilter::Any => true,
            TemporalFilter::Timestamp(t) => vf <= t && t <= vt,
            TemporalFilter::Range(start, end) => vf <= end && vt >= start,
        }
    }
}

fn numeric(v: Option<&Value>) -> Option<f64> {
    match v {
        Some(Value::Int(n)) => Some(*n as f64),
        Some(Value::Float(f)) => Some(*f),
        _ => None,
    }
}

/// BFS traversal that applies a [`TemporalFilter`] to every candidate
/// edge before it is followed. Mirrors the structure of
/// [`crate::Traverse`] but with the filter step inlined.
pub struct TemporalTraverse<'a> {
    pub start_vertex: VertexId,
    pub graph: &'a str,
    pub label: Option<&'a str>,
    pub max_hops: u32,
    pub filter: TemporalFilter,
    pub score: f64,
}

impl<'a> TemporalTraverse<'a> {
    pub fn new(start: VertexId, graph: &'a str) -> Self {
        Self {
            start_vertex: start,
            graph,
            label: None,
            max_hops: 1,
            filter: TemporalFilter::Any,
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

    pub fn filter(mut self, filter: TemporalFilter) -> Self {
        self.filter = filter;
        self
    }

    pub fn execute<G: GraphStore>(&self, store: &G) -> GraphPostingList {
        let mut visited: BTreeSet<VertexId> = BTreeSet::new();
        let mut frontier: BTreeSet<VertexId> = BTreeSet::new();
        frontier.insert(self.start_vertex);
        let mut all_edges: BTreeSet<EdgeId> = BTreeSet::new();

        for _ in 0..self.max_hops {
            let mut next_frontier: BTreeSet<VertexId> = BTreeSet::new();
            for v in &frontier {
                for eid in store.out_edge_ids(*v, self.graph) {
                    let Some(edge) = store.get_edge(eid) else {
                        continue;
                    };
                    if let Some(want) = self.label {
                        if edge.label != want {
                            continue;
                        }
                    }
                    if !self.filter.is_valid(&edge.properties) {
                        continue;
                    }
                    let neighbor = edge.target_id;
                    if !visited.contains(&neighbor) && !frontier.contains(&neighbor) {
                        next_frontier.insert(neighbor);
                    }
                    all_edges.insert(eid);
                }
            }
            visited.append(&mut frontier.clone());
            frontier = next_frontier;
            if frontier.is_empty() {
                break;
            }
        }
        visited.append(&mut frontier);

        let visited_vec: Vec<VertexId> = visited.iter().copied().collect();
        let edges_vec: Vec<EdgeId> = all_edges.iter().copied().collect();
        let mut entries: Vec<PostingEntry> = Vec::with_capacity(visited_vec.len());
        let mut graph_payloads: BTreeMap<DocId, GraphPayload> = BTreeMap::new();
        for vid in &visited_vec {
            entries.push(PostingEntry::new(*vid, Payload::with_score(self.score)));
            graph_payloads.insert(
                *vid,
                GraphPayload {
                    subgraph_vertices: visited_vec.clone(),
                    subgraph_edges: edges_vec.clone(),
                    graph_name: self.graph.to_string(),
                    score_override: Some(self.score),
                },
            );
        }
        GraphPostingList::from_parts(PostingList::from_sorted_unchecked(entries), graph_payloads)
    }
}
