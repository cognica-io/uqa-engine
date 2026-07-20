//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Graph operators: BFS traversal, label match, subgraph isomorphism,
//! and vertex aggregation. Each operator returns a
//! [`GraphPostingList`] so the result composes with the standard
//! posting-list algebra via [`crate::GraphPostingList::to_posting_list`].

use std::collections::{BTreeMap, BTreeSet};

use uqa_core::{DocId, Edge, EdgeId, Payload, PostingEntry, PostingList, Value, VertexId};

use std::collections::VecDeque;

use crate::pattern::{EdgePattern, GraphPattern, VertexPredicate};
use crate::posting_list::{GraphPayload, GraphPostingList};
use crate::rpq::{build_nfa, simplify, subset_construction, Dfa, DfaState, RegularPathExpr};
use crate::store::GraphStore;

/// Default score lifted into the traversal / match payload. The UQA
/// reference uses 0.9 so calibrated fusion treats graph hits as a
/// strong-but-not-certain signal.
pub const DEFAULT_GRAPH_SCORE: f64 = 0.9;

// -------------------------------------------------------------------------
// Traverse
// -------------------------------------------------------------------------

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
                    let neighbor = edge.target_id;
                    if visited.contains(&neighbor) || frontier.contains(&neighbor) {
                        // Already explored or about to be — but still record the edge.
                        all_edges.insert(eid);
                        continue;
                    }
                    if let Some(pred) = &self.vertex_predicate {
                        if let Some(vtx) = store.get_vertex(neighbor) {
                            if !pred.matches(vtx) {
                                continue;
                            }
                        } else {
                            continue;
                        }
                    }
                    next_frontier.insert(neighbor);
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

// -------------------------------------------------------------------------
// VertexMatch
// -------------------------------------------------------------------------

/// Single-vertex Match: every vertex in `graph` whose label matches and
/// whose predicates all hold. Useful as a Cypher-style anchor.
pub struct VertexMatch<'a> {
    pub graph: &'a str,
    pub label: Option<&'a str>,
    pub predicate: Option<VertexPredicate>,
    pub score: f64,
}

impl<'a> VertexMatch<'a> {
    pub fn new(graph: &'a str) -> Self {
        Self {
            graph,
            label: None,
            predicate: None,
            score: DEFAULT_GRAPH_SCORE,
        }
    }

    pub fn label(mut self, label: &'a str) -> Self {
        self.label = Some(label);
        self
    }

    pub fn predicate(mut self, p: VertexPredicate) -> Self {
        self.predicate = Some(p);
        self
    }

    pub fn execute<G: GraphStore>(&self, store: &G) -> GraphPostingList {
        let candidates: Vec<VertexId> = match self.label {
            Some(l) => store.vertex_ids_by_label(l, self.graph),
            None => store.vertex_ids_in_graph(self.graph).into_iter().collect(),
        };
        let mut entries: Vec<PostingEntry> = Vec::new();
        let mut graph_payloads: BTreeMap<DocId, GraphPayload> = BTreeMap::new();
        for vid in candidates {
            let Some(vtx) = store.get_vertex(vid) else {
                continue;
            };
            if let Some(pred) = &self.predicate {
                if !pred.matches(vtx) {
                    continue;
                }
            }
            entries.push(PostingEntry::new(vid, Payload::with_score(self.score)));
            graph_payloads.insert(
                vid,
                GraphPayload {
                    subgraph_vertices: vec![vid],
                    subgraph_edges: Vec::new(),
                    graph_name: self.graph.to_string(),
                    score_override: Some(self.score),
                },
            );
        }
        entries.sort_by_key(|e| e.doc_id);
        GraphPostingList::from_parts(PostingList::from_sorted_unchecked(entries), graph_payloads)
    }
}

// -------------------------------------------------------------------------
// GMatch (subgraph isomorphism)
// -------------------------------------------------------------------------

/// Backtracking working set: which variables remain to be bound, the
/// current partial assignment, the set of already-bound vertex ids, and
/// the accumulator for completed matches.
struct BacktrackState {
    unassigned: BTreeSet<String>,
    assignment: BTreeMap<String, VertexId>,
    assigned_values: BTreeSet<VertexId>,
    matches: Vec<BTreeMap<String, VertexId>>,
}

/// `GMatch_P` (Definition 2.2.2 / 5.2.2): subgraph-isomorphism pattern
/// matching via backtracking with arc-consistency candidate pruning,
/// MRV (minimum remaining values) variable ordering, and a negated-edge
/// post-filter. Each match maps each pattern variable to a vertex id;
/// the result is a `GraphPostingList` keyed on a synthetic 1-based
/// match id, with the assignment carried as payload fields.
pub struct GMatch<'a> {
    pub pattern: GraphPattern,
    pub graph: &'a str,
    pub score: f64,
}

impl<'a> GMatch<'a> {
    pub fn new(pattern: GraphPattern, graph: &'a str) -> Self {
        Self {
            pattern,
            graph,
            score: DEFAULT_GRAPH_SCORE,
        }
    }

    pub fn execute<G: GraphStore>(&self, store: &G) -> GraphPostingList {
        if let Some(result) = self.try_execute_single_edge(store) {
            return result;
        }
        if let Some(result) = self.try_execute_two_edge_path(store) {
            return result;
        }

        let candidates = self.compute_candidates(store);

        let (positive_edges, negated_edges): (Vec<&EdgePattern>, Vec<&EdgePattern>) =
            self.pattern.edge_patterns.iter().partition(|e| !e.negated);

        let mut var_edges: BTreeMap<String, Vec<&EdgePattern>> = BTreeMap::new();
        for ep in &positive_edges {
            var_edges.entry(ep.source_var.clone()).or_default().push(ep);
            var_edges.entry(ep.target_var.clone()).or_default().push(ep);
        }

        let variables: Vec<String> = self
            .pattern
            .vertex_patterns
            .iter()
            .map(|vp| vp.variable.clone())
            .collect();
        let mut state = BacktrackState {
            unassigned: variables.iter().cloned().collect(),
            assignment: BTreeMap::new(),
            assigned_values: BTreeSet::new(),
            matches: Vec::new(),
        };
        self.backtrack(store, &candidates, &var_edges, &mut state);
        let mut matches = state.matches;

        if !negated_edges.is_empty() {
            matches.retain(|m| Self::check_negated(store, self.graph, &negated_edges, m));
        }

        let mut entries: Vec<PostingEntry> = Vec::with_capacity(matches.len());
        let mut graph_payloads: BTreeMap<DocId, GraphPayload> = BTreeMap::new();
        for (i, m) in matches.iter().enumerate() {
            let doc_id = (i + 1) as DocId;
            let mut fields = BTreeMap::new();
            for (var, vid) in m {
                fields.insert(var.clone(), Value::Int(*vid as i64));
            }
            entries.push(PostingEntry::new(
                doc_id,
                Payload {
                    positions: Vec::new(),
                    score: self.score,
                    fields,
                },
            ));
            let mut subgraph_vertices: Vec<VertexId> = m.values().copied().collect();
            subgraph_vertices.sort_unstable();
            subgraph_vertices.dedup();
            let subgraph_edges = Self::collect_match_edges(store, self.graph, &positive_edges, m);
            graph_payloads.insert(
                doc_id,
                GraphPayload {
                    subgraph_vertices,
                    subgraph_edges,
                    graph_name: self.graph.to_string(),
                    score_override: Some(self.score),
                },
            );
        }
        GraphPostingList::from_parts(PostingList::from_sorted_unchecked(entries), graph_payloads)
    }

    fn try_execute_single_edge<G: GraphStore>(&self, store: &G) -> Option<GraphPostingList> {
        if self.pattern.vertex_patterns.len() != 2 || self.pattern.edge_patterns.len() != 1 {
            return None;
        }
        let edge_pattern = &self.pattern.edge_patterns[0];
        if edge_pattern.negated || edge_pattern.source_var == edge_pattern.target_var {
            return None;
        }
        let source_pattern = self.vertex_pattern(&edge_pattern.source_var)?;
        let target_pattern = self.vertex_pattern(&edge_pattern.target_var)?;
        let edge_list = self.edge_list_for_pattern(store, edge_pattern);

        let mut seen_assignments: BTreeSet<(VertexId, VertexId)> = BTreeSet::new();
        let mut entries: Vec<PostingEntry> = Vec::new();
        let mut graph_payloads: BTreeMap<DocId, GraphPayload> = BTreeMap::new();
        for edge in edge_list {
            if !edge_pattern.satisfies(&edge) {
                continue;
            }
            let Some(source) = store.get_vertex(edge.source_id) else {
                continue;
            };
            if !source_pattern.satisfies(source) {
                continue;
            }
            let Some(target) = store.get_vertex(edge.target_id) else {
                continue;
            };
            if !target_pattern.satisfies(target) {
                continue;
            }
            if !seen_assignments.insert((edge.source_id, edge.target_id)) {
                continue;
            }

            let doc_id = (entries.len() + 1) as DocId;
            let mut fields = BTreeMap::new();
            fields.insert(
                edge_pattern.source_var.clone(),
                Value::Int(edge.source_id as i64),
            );
            fields.insert(
                edge_pattern.target_var.clone(),
                Value::Int(edge.target_id as i64),
            );
            entries.push(PostingEntry::new(
                doc_id,
                Payload {
                    positions: Vec::new(),
                    score: self.score,
                    fields,
                },
            ));
            let mut subgraph_vertices = vec![edge.source_id, edge.target_id];
            subgraph_vertices.sort_unstable();
            subgraph_vertices.dedup();
            graph_payloads.insert(
                doc_id,
                GraphPayload {
                    subgraph_vertices,
                    subgraph_edges: vec![edge.edge_id],
                    graph_name: self.graph.to_string(),
                    score_override: Some(self.score),
                },
            );
        }

        Some(GraphPostingList::from_parts(
            PostingList::from_sorted_unchecked(entries),
            graph_payloads,
        ))
    }

    fn try_execute_two_edge_path<G: GraphStore>(&self, store: &G) -> Option<GraphPostingList> {
        if self.pattern.vertex_patterns.len() != 3 || self.pattern.edge_patterns.len() != 2 {
            return None;
        }
        let edge_patterns = &self.pattern.edge_patterns;
        if edge_patterns.iter().any(|edge| edge.negated) {
            return None;
        }
        let (first, second) = if edge_patterns[0].target_var == edge_patterns[1].source_var {
            (&edge_patterns[0], &edge_patterns[1])
        } else if edge_patterns[1].target_var == edge_patterns[0].source_var {
            (&edge_patterns[1], &edge_patterns[0])
        } else {
            return None;
        };
        if first.source_var == first.target_var
            || second.source_var == second.target_var
            || first.source_var == second.target_var
        {
            return None;
        }
        let source_pattern = self.vertex_pattern(&first.source_var)?;
        let middle_pattern = self.vertex_pattern(&first.target_var)?;
        let target_pattern = self.vertex_pattern(&second.target_var)?;

        let first_edges = self.edge_list_for_pattern(store, first);
        let mut seen_assignments: BTreeSet<(VertexId, VertexId, VertexId)> = BTreeSet::new();
        let mut entries: Vec<PostingEntry> = Vec::new();
        let mut graph_payloads: BTreeMap<DocId, GraphPayload> = BTreeMap::new();

        for first_edge in first_edges {
            if !first.satisfies(&first_edge) {
                continue;
            }
            let Some(source) = store.get_vertex(first_edge.source_id) else {
                continue;
            };
            if !source_pattern.satisfies(source) {
                continue;
            }
            let Some(middle) = store.get_vertex(first_edge.target_id) else {
                continue;
            };
            if !middle_pattern.satisfies(middle) {
                continue;
            }

            for second_edge_id in store.out_edge_ids(first_edge.target_id, self.graph) {
                let Some(second_edge) = store.get_edge(second_edge_id) else {
                    continue;
                };
                if !second.satisfies(second_edge) {
                    continue;
                }
                if first_edge.source_id == second_edge.target_id
                    || first_edge.target_id == second_edge.target_id
                {
                    continue;
                }
                let Some(target) = store.get_vertex(second_edge.target_id) else {
                    continue;
                };
                if !target_pattern.satisfies(target) {
                    continue;
                }
                let assignment = (
                    first_edge.source_id,
                    first_edge.target_id,
                    second_edge.target_id,
                );
                if !seen_assignments.insert(assignment) {
                    continue;
                }
                self.push_two_edge_path_match(
                    first,
                    second,
                    &first_edge,
                    second_edge,
                    &mut entries,
                    &mut graph_payloads,
                );
            }
        }

        Some(GraphPostingList::from_parts(
            PostingList::from_sorted_unchecked(entries),
            graph_payloads,
        ))
    }

    fn push_two_edge_path_match(
        &self,
        first: &EdgePattern,
        second: &EdgePattern,
        first_edge: &Edge,
        second_edge: &Edge,
        entries: &mut Vec<PostingEntry>,
        graph_payloads: &mut BTreeMap<DocId, GraphPayload>,
    ) {
        let doc_id = (entries.len() + 1) as DocId;
        let mut fields = BTreeMap::new();
        fields.insert(
            first.source_var.clone(),
            Value::Int(first_edge.source_id as i64),
        );
        fields.insert(
            first.target_var.clone(),
            Value::Int(first_edge.target_id as i64),
        );
        fields.insert(
            second.target_var.clone(),
            Value::Int(second_edge.target_id as i64),
        );
        entries.push(PostingEntry::new(
            doc_id,
            Payload {
                positions: Vec::new(),
                score: self.score,
                fields,
            },
        ));

        let mut subgraph_edges = vec![first_edge.edge_id, second_edge.edge_id];
        subgraph_edges.sort_unstable();
        subgraph_edges.dedup();
        let mut subgraph_vertices = vec![
            first_edge.source_id,
            first_edge.target_id,
            second_edge.target_id,
        ];
        subgraph_vertices.sort_unstable();
        subgraph_vertices.dedup();
        graph_payloads.insert(
            doc_id,
            GraphPayload {
                subgraph_vertices,
                subgraph_edges,
                graph_name: self.graph.to_string(),
                score_override: Some(self.score),
            },
        );
    }

    fn edge_list_for_pattern<G: GraphStore>(&self, store: &G, pattern: &EdgePattern) -> Vec<Edge> {
        match pattern.label.as_deref() {
            Some(label) => store
                .edge_ids_by_label(label, self.graph)
                .into_iter()
                .filter_map(|edge_id| store.get_edge(edge_id).cloned())
                .collect(),
            None => store.edges_in_graph(self.graph),
        }
    }

    fn vertex_pattern(&self, variable: &str) -> Option<&crate::pattern::VertexPattern> {
        self.pattern
            .vertex_patterns
            .iter()
            .find(|pattern| pattern.variable == variable)
    }

    fn compute_candidates<G: GraphStore>(&self, store: &G) -> BTreeMap<String, Vec<VertexId>> {
        let mut candidates: BTreeMap<String, Vec<VertexId>> = BTreeMap::new();
        let graph_vids = store.vertex_ids_in_graph(self.graph);
        for vp in &self.pattern.vertex_patterns {
            let mut cands = Vec::new();
            for vid in &graph_vids {
                if let Some(vtx) = store.get_vertex(*vid) {
                    if vp.satisfies(vtx) {
                        cands.push(*vid);
                    }
                }
            }
            candidates.insert(vp.variable.clone(), cands);
        }

        // Arc consistency pass — skip negated edges (post-filtered).
        let mut changed = true;
        while changed {
            changed = false;
            for ep in &self.pattern.edge_patterns {
                if ep.negated {
                    continue;
                }
                let (src_var, tgt_var) = (&ep.source_var, &ep.target_var);
                let Some(_src_cands) = candidates.get(src_var) else {
                    continue;
                };
                let Some(tgt_cands) = candidates.get(tgt_var) else {
                    continue;
                };
                let tgt_set: BTreeSet<VertexId> = tgt_cands.iter().copied().collect();
                let new_src: Vec<VertexId> = candidates[src_var]
                    .iter()
                    .copied()
                    .filter(|vid| Self::has_edge_out(store, self.graph, *vid, &tgt_set, ep))
                    .collect();
                if new_src.len() < candidates[src_var].len() {
                    candidates.insert(src_var.clone(), new_src);
                    changed = true;
                }
                let src_set: BTreeSet<VertexId> = candidates[src_var].iter().copied().collect();
                let new_tgt: Vec<VertexId> = candidates[tgt_var]
                    .iter()
                    .copied()
                    .filter(|vid| Self::has_edge_in(store, self.graph, *vid, &src_set, ep))
                    .collect();
                if new_tgt.len() < candidates[tgt_var].len() {
                    candidates.insert(tgt_var.clone(), new_tgt);
                    changed = true;
                }
            }
        }
        candidates
    }

    fn has_edge_out<G: GraphStore>(
        store: &G,
        graph: &str,
        src: VertexId,
        tgt_set: &BTreeSet<VertexId>,
        ep: &EdgePattern,
    ) -> bool {
        for eid in store.out_edge_ids(src, graph) {
            let Some(edge) = store.get_edge(eid) else {
                continue;
            };
            if !tgt_set.contains(&edge.target_id) {
                continue;
            }
            if ep.satisfies(edge) {
                return true;
            }
        }
        false
    }

    fn has_edge_in<G: GraphStore>(
        store: &G,
        graph: &str,
        tgt: VertexId,
        src_set: &BTreeSet<VertexId>,
        ep: &EdgePattern,
    ) -> bool {
        for eid in store.in_edge_ids(tgt, graph) {
            let Some(edge) = store.get_edge(eid) else {
                continue;
            };
            if !src_set.contains(&edge.source_id) {
                continue;
            }
            if ep.satisfies(edge) {
                return true;
            }
        }
        false
    }

    fn backtrack<G: GraphStore>(
        &self,
        store: &G,
        candidates: &BTreeMap<String, Vec<VertexId>>,
        var_edges: &BTreeMap<String, Vec<&EdgePattern>>,
        state: &mut BacktrackState,
    ) {
        if state.unassigned.is_empty() {
            state.matches.push(state.assignment.clone());
            return;
        }
        let var = state
            .unassigned
            .iter()
            .min_by_key(|v| candidates.get(*v).map_or(usize::MAX, Vec::len))
            .cloned()
            .expect("unassigned non-empty");
        let cands = candidates.get(&var).cloned().unwrap_or_default();
        for vid in cands {
            if state.assigned_values.contains(&vid) {
                continue;
            }
            state.assignment.insert(var.clone(), vid);
            state.assigned_values.insert(vid);
            state.unassigned.remove(&var);
            if Self::validate_edges_for(store, self.graph, &var, var_edges, &state.assignment) {
                self.backtrack(store, candidates, var_edges, state);
            }
            state.assignment.remove(&var);
            state.assigned_values.remove(&vid);
            state.unassigned.insert(var.clone());
        }
    }

    fn validate_edges_for<G: GraphStore>(
        store: &G,
        graph: &str,
        var: &str,
        var_edges: &BTreeMap<String, Vec<&EdgePattern>>,
        assignment: &BTreeMap<String, VertexId>,
    ) -> bool {
        let Some(edges) = var_edges.get(var) else {
            return true;
        };
        for ep in edges {
            let (Some(src_id), Some(tgt_id)) = (
                assignment.get(&ep.source_var).copied(),
                assignment.get(&ep.target_var).copied(),
            ) else {
                continue;
            };
            let mut found = false;
            for eid in store.out_edge_ids(src_id, graph) {
                let Some(edge) = store.get_edge(eid) else {
                    continue;
                };
                if edge.target_id != tgt_id {
                    continue;
                }
                if ep.satisfies(edge) {
                    found = true;
                    break;
                }
            }
            if !found {
                return false;
            }
        }
        true
    }

    fn check_negated<G: GraphStore>(
        store: &G,
        graph: &str,
        negated: &[&EdgePattern],
        assignment: &BTreeMap<String, VertexId>,
    ) -> bool {
        for ep in negated {
            let (Some(src_id), Some(tgt_id)) = (
                assignment.get(&ep.source_var).copied(),
                assignment.get(&ep.target_var).copied(),
            ) else {
                continue;
            };
            for eid in store.out_edge_ids(src_id, graph) {
                let Some(edge) = store.get_edge(eid) else {
                    continue;
                };
                if edge.target_id != tgt_id {
                    continue;
                }
                if ep.satisfies(edge) {
                    return false;
                }
            }
        }
        true
    }

    fn collect_match_edges<G: GraphStore>(
        store: &G,
        graph: &str,
        positive: &[&EdgePattern],
        assignment: &BTreeMap<String, VertexId>,
    ) -> Vec<EdgeId> {
        let mut edges: BTreeSet<EdgeId> = BTreeSet::new();
        for ep in positive {
            let (Some(src_id), Some(tgt_id)) = (
                assignment.get(&ep.source_var).copied(),
                assignment.get(&ep.target_var).copied(),
            ) else {
                continue;
            };
            for eid in store.out_edge_ids(src_id, graph) {
                let Some(edge) = store.get_edge(eid) else {
                    continue;
                };
                if edge.target_id == tgt_id && ep.satisfies(edge) {
                    edges.insert(eid);
                    break;
                }
            }
        }
        edges.into_iter().collect()
    }
}

// -------------------------------------------------------------------------
// VertexAggregation
// -------------------------------------------------------------------------

/// Aggregation function over a numeric vertex property.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggFn {
    Sum,
    Avg,
    Min,
    Max,
    Count,
}

/// Aggregate a numeric property over the vertex set rolled up in a
/// source operator's `GraphPayload`s. Mirrors Definition 2.2.3.
pub struct VertexAggregation<'a> {
    pub source: GraphPostingList,
    pub property_name: String,
    pub agg_fn: AggFn,
    pub graph: &'a str,
}

impl<'a> VertexAggregation<'a> {
    pub fn new(
        source: GraphPostingList,
        property: impl Into<String>,
        agg_fn: AggFn,
        graph: &'a str,
    ) -> Self {
        Self {
            source,
            property_name: property.into(),
            agg_fn,
            graph,
        }
    }

    pub fn execute<G: GraphStore>(&self, store: &G) -> GraphPostingList {
        let mut vertex_ids: BTreeSet<VertexId> = BTreeSet::new();
        for entry in self.source.inner().entries() {
            if let Some(gp) = self.source.get_graph_payload(entry.doc_id) {
                vertex_ids.extend(gp.subgraph_vertices.iter().copied());
            }
        }
        let mut numeric: Vec<f64> = Vec::new();
        for vid in &vertex_ids {
            if let Some(vtx) = store.get_vertex(*vid) {
                if let Some(value) = vtx.properties.get(&self.property_name) {
                    if let Some(n) = value_as_f64(value) {
                        numeric.push(n);
                    }
                }
            }
        }
        let result = aggregate(self.agg_fn, &numeric);

        let mut fields: BTreeMap<String, Value> = BTreeMap::new();
        fields.insert(
            "_vertex_agg_property".to_string(),
            Value::Str(self.property_name.clone()),
        );
        fields.insert(
            "_vertex_agg_fn".to_string(),
            Value::Str(format!("{:?}", self.agg_fn).to_lowercase()),
        );
        fields.insert("_vertex_agg_result".to_string(), Value::Float(result));
        fields.insert(
            "_vertex_agg_count".to_string(),
            Value::Int(numeric.len() as i64),
        );

        let entry = PostingEntry::new(
            0,
            Payload {
                positions: Vec::new(),
                score: result,
                fields,
            },
        );
        let mut graph_payloads = BTreeMap::new();
        graph_payloads.insert(
            0,
            GraphPayload {
                subgraph_vertices: vertex_ids.into_iter().collect(),
                subgraph_edges: Vec::new(),
                graph_name: self.graph.to_string(),
                score_override: Some(result),
            },
        );
        GraphPostingList::from_parts(
            PostingList::from_sorted_unchecked(vec![entry]),
            graph_payloads,
        )
    }
}

// -------------------------------------------------------------------------
// RegularPathQuery (RPQ_R)
// -------------------------------------------------------------------------

/// `RPQ_R` (Definition 5.1.2): evaluate a regular path expression over
/// a graph. The expression is simplified, compiled to an NFA via
/// Thompson's construction, then converted to a DFA and simulated by a
/// BFS over `(vertex, dfa-state)` configurations.
///
/// The result lists every endpoint vertex reachable from a start
/// vertex along a path matching the expression. Each endpoint becomes
/// one entry in the returned `GraphPostingList`.
pub struct RegularPathQuery<'a> {
    pub path: RegularPathExpr,
    pub graph: &'a str,
    /// `Some(start)` restricts evaluation to a single source. `None`
    /// runs the query from every vertex in the graph.
    pub start_vertex: Option<VertexId>,
    pub score: f64,
}

impl<'a> RegularPathQuery<'a> {
    pub fn new(path: RegularPathExpr, graph: &'a str) -> Self {
        Self {
            path,
            graph,
            start_vertex: None,
            score: DEFAULT_GRAPH_SCORE,
        }
    }

    pub fn from_vertex(mut self, start: VertexId) -> Self {
        self.start_vertex = Some(start);
        self
    }

    pub fn execute<G: GraphStore>(&self, store: &G) -> GraphPostingList {
        let simplified = simplify(&self.path);
        let nfa = build_nfa(&simplified);
        let dfa = subset_construction(&nfa);

        let starts: Vec<VertexId> = match self.start_vertex {
            Some(v) => vec![v],
            None => store.vertex_ids_in_graph(self.graph).into_iter().collect(),
        };

        let mut pairs: BTreeSet<(VertexId, VertexId)> = BTreeSet::new();
        for sv in &starts {
            self.simulate_from(store, *sv, &dfa, &mut pairs);
        }

        let mut entries: Vec<PostingEntry> = Vec::new();
        let mut graph_payloads: BTreeMap<DocId, GraphPayload> = BTreeMap::new();
        let mut seen: BTreeSet<DocId> = BTreeSet::new();
        for (start_v, end_v) in &pairs {
            let doc_id = *end_v as DocId;
            if seen.insert(doc_id) {
                entries.push(PostingEntry::new(doc_id, Payload::with_score(self.score)));
                let mut subgraph_vertices = vec![*start_v, *end_v];
                subgraph_vertices.sort_unstable();
                subgraph_vertices.dedup();
                graph_payloads.insert(
                    doc_id,
                    GraphPayload {
                        subgraph_vertices,
                        subgraph_edges: Vec::new(),
                        graph_name: self.graph.to_string(),
                        score_override: Some(self.score),
                    },
                );
            }
        }
        entries.sort_by_key(|e| e.doc_id);
        GraphPostingList::from_parts(PostingList::from_sorted_unchecked(entries), graph_payloads)
    }

    fn simulate_from<G: GraphStore>(
        &self,
        store: &G,
        start: VertexId,
        dfa: &Dfa,
        pairs: &mut BTreeSet<(VertexId, VertexId)>,
    ) {
        let mut visited: BTreeSet<(VertexId, DfaState)> = BTreeSet::new();
        let mut queue: VecDeque<(VertexId, DfaState)> = VecDeque::new();
        queue.push_back((start, dfa.start.clone()));
        visited.insert((start, dfa.start.clone()));

        if dfa.accepts.contains(&dfa.start) {
            pairs.insert((start, start));
        }

        while let Some((vertex, state)) = queue.pop_front() {
            let Some(transitions) = dfa.transitions.get(&state) else {
                continue;
            };
            for eid in store.out_edge_ids(vertex, self.graph) {
                let Some(edge) = store.get_edge(eid) else {
                    continue;
                };
                let Some(next_state) = transitions.get(&edge.label) else {
                    continue;
                };
                let neighbor = edge.target_id;
                if dfa.accepts.contains(next_state) {
                    pairs.insert((start, neighbor));
                }
                let key = (neighbor, next_state.clone());
                if visited.insert(key.clone()) {
                    queue.push_back(key);
                }
            }
        }
    }
}

fn value_as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Int(n) => Some(*n as f64),
        Value::Float(f) => Some(*f),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

fn aggregate(agg_fn: AggFn, values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    match agg_fn {
        AggFn::Sum => values.iter().sum(),
        AggFn::Avg => values.iter().sum::<f64>() / values.len() as f64,
        AggFn::Min => values.iter().copied().fold(f64::INFINITY, f64::min),
        AggFn::Max => values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        AggFn::Count => values.len() as f64,
    }
}
