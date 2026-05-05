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
use crate::pattern::GraphPattern;
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

/// Temporal-aware pattern matching (Section 10, Paper 2). Mirrors
/// `uqa.graph.temporal_pattern_match.TemporalPatternMatchOperator`.
///
/// Same algorithm as the standard subgraph matcher but every edge
/// candidate is filtered through a [`TemporalFilter`] before it is
/// admitted into the assignment, so only temporally valid edges
/// participate in pattern matching.
pub struct TemporalPatternMatch<'a> {
    pub pattern: GraphPattern,
    pub graph: &'a str,
    pub temporal_filter: TemporalFilter,
    pub score: f64,
}

impl<'a> TemporalPatternMatch<'a> {
    pub fn new(pattern: GraphPattern, graph: &'a str) -> Self {
        Self {
            pattern,
            graph,
            temporal_filter: TemporalFilter::Any,
            score: DEFAULT_GRAPH_SCORE,
        }
    }

    pub fn filter(mut self, filter: TemporalFilter) -> Self {
        self.temporal_filter = filter;
        self
    }

    pub fn score(mut self, score: f64) -> Self {
        self.score = score;
        self
    }

    pub fn execute<G: GraphStore>(&self, store: &G) -> GraphPostingList {
        let candidates = self.compute_candidates(store);

        // Group edges by both source and target variable so the
        // backtracking validator can quickly find every edge that
        // touches a newly-assigned variable.
        let mut var_edges: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, ep) in self.pattern.edge_patterns.iter().enumerate() {
            var_edges.entry(ep.source_var.clone()).or_default().push(i);
            var_edges.entry(ep.target_var.clone()).or_default().push(i);
        }

        let mut unassigned: BTreeSet<String> = self
            .pattern
            .vertex_patterns
            .iter()
            .map(|vp| vp.variable.clone())
            .collect();
        let mut assignment: BTreeMap<String, VertexId> = BTreeMap::new();
        let mut assigned_values: BTreeSet<VertexId> = BTreeSet::new();
        let mut matches: Vec<BTreeMap<String, VertexId>> = Vec::new();

        self.backtrack(
            store,
            &candidates,
            &var_edges,
            &mut unassigned,
            &mut assignment,
            &mut assigned_values,
            &mut matches,
        );

        let mut entries: Vec<PostingEntry> = Vec::with_capacity(matches.len());
        let mut graph_payloads: BTreeMap<DocId, GraphPayload> = BTreeMap::new();
        for (i, assn) in matches.iter().enumerate() {
            let doc_id = (i as u64) + 1;
            let mut fields: BTreeMap<String, Value> = BTreeMap::new();
            for (k, v) in assn {
                fields.insert(k.clone(), Value::Int(*v as i64));
            }
            entries.push(PostingEntry::new(
                doc_id,
                Payload {
                    score: self.score,
                    fields,
                    ..Default::default()
                },
            ));
            let match_vertices: Vec<VertexId> = assn.values().copied().collect();
            let match_edges = self.collect_match_edges(store, assn);
            graph_payloads.insert(
                doc_id,
                GraphPayload {
                    subgraph_vertices: match_vertices,
                    subgraph_edges: match_edges,
                    graph_name: self.graph.to_string(),
                    score_override: Some(self.score),
                },
            );
        }

        GraphPostingList::from_parts(PostingList::from_sorted_unchecked(entries), graph_payloads)
    }

    fn compute_candidates<G: GraphStore>(&self, store: &G) -> BTreeMap<String, Vec<VertexId>> {
        let mut out: BTreeMap<String, Vec<VertexId>> = BTreeMap::new();
        let vids = store.vertex_ids_in_graph(self.graph);
        for vp in &self.pattern.vertex_patterns {
            let candidates: Vec<VertexId> = vids
                .iter()
                .copied()
                .filter(|vid| {
                    let Some(vertex) = store.get_vertex(*vid) else {
                        return false;
                    };
                    vp.constraints.iter().all(|c| c.matches(vertex))
                })
                .collect();
            out.insert(vp.variable.clone(), candidates);
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn backtrack<G: GraphStore>(
        &self,
        store: &G,
        candidates: &BTreeMap<String, Vec<VertexId>>,
        var_edges: &BTreeMap<String, Vec<usize>>,
        unassigned: &mut BTreeSet<String>,
        assignment: &mut BTreeMap<String, VertexId>,
        assigned_values: &mut BTreeSet<VertexId>,
        matches: &mut Vec<BTreeMap<String, VertexId>>,
    ) {
        if unassigned.is_empty() {
            matches.push(assignment.clone());
            return;
        }
        // Pick the variable with the fewest candidates first (MRV
        // heuristic, same as Python's `min(unassigned, key=lambda v:
        // len(candidates[v]))`).
        let pick: String = unassigned
            .iter()
            .min_by_key(|v| candidates.get(*v).map_or(usize::MAX, Vec::len))
            .cloned()
            .unwrap();

        let cands: Vec<VertexId> = candidates.get(&pick).cloned().unwrap_or_default();
        unassigned.remove(&pick);

        for vid in cands {
            if assigned_values.contains(&vid) {
                continue;
            }
            assignment.insert(pick.clone(), vid);
            assigned_values.insert(vid);

            if self.validate_edges_for(store, &pick, var_edges, assignment) {
                self.backtrack(
                    store,
                    candidates,
                    var_edges,
                    unassigned,
                    assignment,
                    assigned_values,
                    matches,
                );
            }

            assignment.remove(&pick);
            assigned_values.remove(&vid);
        }

        unassigned.insert(pick);
    }

    fn validate_edges_for<G: GraphStore>(
        &self,
        store: &G,
        var: &str,
        var_edges: &BTreeMap<String, Vec<usize>>,
        assignment: &BTreeMap<String, VertexId>,
    ) -> bool {
        let Some(edges) = var_edges.get(var) else {
            return true;
        };
        for &ei in edges {
            let ep = &self.pattern.edge_patterns[ei];
            let (Some(&src_id), Some(&tgt_id)) = (
                assignment.get(&ep.source_var),
                assignment.get(&ep.target_var),
            ) else {
                continue;
            };
            let mut found = false;
            for eid in store.out_edge_ids(src_id, self.graph) {
                let Some(edge) = store.get_edge(eid) else {
                    continue;
                };
                if edge.target_id != tgt_id {
                    continue;
                }
                if let Some(label) = &ep.label {
                    if edge.label != *label {
                        continue;
                    }
                }
                if !ep.constraints.iter().all(|c| c.matches(edge)) {
                    continue;
                }
                if !self.temporal_filter.is_valid(&edge.properties) {
                    continue;
                }
                found = true;
                break;
            }
            if !found {
                return false;
            }
        }
        true
    }

    fn collect_match_edges<G: GraphStore>(
        &self,
        store: &G,
        assignment: &BTreeMap<String, VertexId>,
    ) -> Vec<EdgeId> {
        let mut edge_ids: BTreeSet<EdgeId> = BTreeSet::new();
        for ep in &self.pattern.edge_patterns {
            let (Some(&src_id), Some(&tgt_id)) = (
                assignment.get(&ep.source_var),
                assignment.get(&ep.target_var),
            ) else {
                continue;
            };
            for eid in store.out_edge_ids(src_id, self.graph) {
                let Some(edge) = store.get_edge(eid) else {
                    continue;
                };
                if edge.target_id == tgt_id
                    && (ep.label.as_deref().is_none_or(|l| edge.label == l))
                    && self.temporal_filter.is_valid(&edge.properties)
                {
                    edge_ids.insert(eid);
                    break;
                }
            }
        }
        edge_ids.into_iter().collect()
    }
}
