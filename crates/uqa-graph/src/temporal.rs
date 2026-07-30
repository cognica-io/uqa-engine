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
use crate::store::{GraphStore, GraphStoreError, GraphStoreResult};

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
    /// Accept edges that are valid at `timestamp` and whose validity
    /// interval overlaps the closed range. Keeping the conjunction in
    /// the physical filter preserves an IR that supplies both bounds.
    TimestampAndRange(f64, f64, f64),
}

impl TemporalFilter {
    pub fn is_valid(&self, properties: &BTreeMap<String, Value>) -> GraphStoreResult<bool> {
        self.validate()?;
        let valid_from = numeric(properties.get("valid_from"), "valid_from")?;
        let valid_to = numeric(properties.get("valid_to"), "valid_to")?;
        if valid_from.is_none() && valid_to.is_none() {
            return Ok(true);
        }
        let vf = valid_from.unwrap_or(f64::NEG_INFINITY);
        let vt = valid_to.unwrap_or(f64::INFINITY);
        Ok(match *self {
            TemporalFilter::Any => true,
            TemporalFilter::Timestamp(t) => vf <= t && t <= vt,
            TemporalFilter::Range(start, end) => vf <= end && vt >= start,
            TemporalFilter::TimestampAndRange(t, start, end) => {
                vf <= t && t <= vt && vf <= end && vt >= start
            }
        })
    }

    fn validate(&self) -> GraphStoreResult<()> {
        let invalid = match *self {
            TemporalFilter::Any => false,
            TemporalFilter::Timestamp(timestamp) => !timestamp.is_finite(),
            TemporalFilter::Range(start, end) => {
                !start.is_finite() || !end.is_finite() || start > end
            }
            TemporalFilter::TimestampAndRange(timestamp, start, end) => {
                !timestamp.is_finite() || !start.is_finite() || !end.is_finite() || start > end
            }
        };
        if invalid {
            Err(GraphStoreError::InvalidMutation(format!(
                "invalid temporal filter {self:?}"
            )))
        } else {
            Ok(())
        }
    }
}

fn numeric(v: Option<&Value>, property: &str) -> GraphStoreResult<Option<f64>> {
    match v {
        None => Ok(None),
        Some(Value::Int(n)) if n.unsigned_abs() <= (1_u64 << 53) => Ok(Some(*n as f64)),
        Some(Value::Float(value)) if value.is_finite() => Ok(Some(*value)),
        Some(Value::Decimal(value)) => value
            .to_f64()
            .filter(|converted| converted.is_finite())
            .map(Some)
            .ok_or_else(|| {
                GraphStoreError::InvalidMutation(format!(
                    "temporal property {property:?} is not representable as finite f64"
                ))
            }),
        Some(Value::Int(value)) => Err(GraphStoreError::InvalidMutation(format!(
            "temporal property {property:?} integer {value} is not exactly representable as f64"
        ))),
        Some(value) => Err(GraphStoreError::InvalidMutation(format!(
            "temporal property {property:?} must be finite numeric, got {value:?}"
        ))),
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

    pub fn execute<G: GraphStore>(&self, store: &G) -> GraphStoreResult<GraphPostingList> {
        self.filter.validate()?;
        validate_score(self.score)?;
        store.require_vertex_in_graph(self.start_vertex, self.graph)?;
        let mut visited: BTreeSet<VertexId> = BTreeSet::new();
        let mut frontier: BTreeSet<VertexId> = BTreeSet::new();
        frontier.insert(self.start_vertex);
        let mut all_edges: BTreeSet<EdgeId> = BTreeSet::new();

        for _ in 0..self.max_hops {
            let mut next_frontier: BTreeSet<VertexId> = BTreeSet::new();
            for v in &frontier {
                for eid in store.out_edge_ids(*v, self.graph)? {
                    let edge = store.get_edge(eid).ok_or_else(|| {
                        GraphStoreError::CorruptGraph(format!(
                            "temporal traversal references missing edge {eid}"
                        ))
                    })?;
                    if let Some(want) = self.label {
                        if edge.label != want {
                            continue;
                        }
                    }
                    if !self.filter.is_valid(&edge.properties)? {
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
        Ok(GraphPostingList::from_parts(
            PostingList::from_sorted_unchecked(entries),
            graph_payloads,
        ))
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

    pub fn execute<G: GraphStore>(&self, store: &G) -> GraphStoreResult<GraphPostingList> {
        self.temporal_filter.validate()?;
        validate_score(self.score)?;
        let candidates = self.compute_candidates(store)?;

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
        )?;

        let mut entries: Vec<PostingEntry> = Vec::with_capacity(matches.len());
        let mut graph_payloads: BTreeMap<DocId, GraphPayload> = BTreeMap::new();
        for (i, assn) in matches.iter().enumerate() {
            let doc_id = u64::try_from(i)
                .ok()
                .and_then(|value| value.checked_add(1))
                .ok_or_else(|| {
                    GraphStoreError::IdExhausted(
                        "temporal match result id counter overflow".to_string(),
                    )
                })?;
            let mut fields: BTreeMap<String, Value> = BTreeMap::new();
            for (k, v) in assn {
                fields.insert(k.clone(), graph_id_value(*v));
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
            let match_edges = self.collect_match_edges(store, assn)?;
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

        Ok(GraphPostingList::from_parts(
            PostingList::from_sorted_unchecked(entries),
            graph_payloads,
        ))
    }

    fn compute_candidates<G: GraphStore>(
        &self,
        store: &G,
    ) -> GraphStoreResult<BTreeMap<String, Vec<VertexId>>> {
        let mut out: BTreeMap<String, Vec<VertexId>> = BTreeMap::new();
        let vids = store.vertex_ids_in_graph(self.graph)?;
        for vp in &self.pattern.vertex_patterns {
            let mut candidates = Vec::new();
            for vid in &vids {
                let vertex = store.get_vertex(*vid).ok_or_else(|| {
                    GraphStoreError::CorruptGraph(format!(
                        "graph {:?} references missing vertex {vid}",
                        self.graph
                    ))
                })?;
                if vp
                    .constraints
                    .iter()
                    .all(|constraint| constraint.matches(vertex))
                {
                    candidates.push(*vid);
                }
            }
            out.insert(vp.variable.clone(), candidates);
        }
        Ok(out)
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
    ) -> GraphStoreResult<()> {
        if unassigned.is_empty() {
            matches.push(assignment.clone());
            return Ok(());
        }
        // Pick the variable with the fewest candidates first (MRV
        // heuristic, same as the canonical UQA implementation's `min(unassigned, key=lambda v:
        // len(candidates[v]))`).
        let pick: String = unassigned
            .iter()
            .min_by_key(|v| candidates.get(*v).map_or(usize::MAX, Vec::len))
            .cloned()
            .ok_or_else(|| {
                GraphStoreError::CorruptGraph(
                    "temporal matcher has no variable to assign".to_string(),
                )
            })?;

        let cands: Vec<VertexId> = candidates.get(&pick).cloned().ok_or_else(|| {
            GraphStoreError::CorruptGraph(format!(
                "temporal matcher has no candidates entry for variable {pick:?}"
            ))
        })?;
        unassigned.remove(&pick);

        for vid in cands {
            if assigned_values.contains(&vid) {
                continue;
            }
            assignment.insert(pick.clone(), vid);
            assigned_values.insert(vid);

            if self.validate_edges_for(store, &pick, var_edges, assignment)? {
                self.backtrack(
                    store,
                    candidates,
                    var_edges,
                    unassigned,
                    assignment,
                    assigned_values,
                    matches,
                )?;
            }

            assignment.remove(&pick);
            assigned_values.remove(&vid);
        }

        unassigned.insert(pick);
        Ok(())
    }

    fn validate_edges_for<G: GraphStore>(
        &self,
        store: &G,
        var: &str,
        var_edges: &BTreeMap<String, Vec<usize>>,
        assignment: &BTreeMap<String, VertexId>,
    ) -> GraphStoreResult<bool> {
        let Some(edges) = var_edges.get(var) else {
            return Ok(true);
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
            for eid in store.out_edge_ids(src_id, self.graph)? {
                let edge = store.get_edge(eid).ok_or_else(|| {
                    GraphStoreError::CorruptGraph(format!(
                        "temporal matcher references missing edge {eid}"
                    ))
                })?;
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
                if !self.temporal_filter.is_valid(&edge.properties)? {
                    continue;
                }
                found = true;
                break;
            }
            if !found {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn collect_match_edges<G: GraphStore>(
        &self,
        store: &G,
        assignment: &BTreeMap<String, VertexId>,
    ) -> GraphStoreResult<Vec<EdgeId>> {
        let mut edge_ids: BTreeSet<EdgeId> = BTreeSet::new();
        for ep in &self.pattern.edge_patterns {
            let (Some(&src_id), Some(&tgt_id)) = (
                assignment.get(&ep.source_var),
                assignment.get(&ep.target_var),
            ) else {
                continue;
            };
            for eid in store.out_edge_ids(src_id, self.graph)? {
                let edge = store.get_edge(eid).ok_or_else(|| {
                    GraphStoreError::CorruptGraph(format!(
                        "temporal match result references missing edge {eid}"
                    ))
                })?;
                if edge.target_id == tgt_id
                    && (ep.label.as_deref().is_none_or(|l| edge.label == l))
                    && self.temporal_filter.is_valid(&edge.properties)?
                {
                    edge_ids.insert(eid);
                    break;
                }
            }
        }
        Ok(edge_ids.into_iter().collect())
    }
}

fn validate_score(score: f64) -> GraphStoreResult<()> {
    if score.is_finite() {
        Ok(())
    } else {
        Err(GraphStoreError::InvalidMutation(
            "temporal graph score must be finite".to_string(),
        ))
    }
}

fn graph_id_value(id: u64) -> Value {
    i64::try_from(id).map_or_else(|_| Value::Bytes(id.to_be_bytes().to_vec()), Value::Int)
}
