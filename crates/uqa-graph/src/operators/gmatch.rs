//! Constraint-pruned subgraph-isomorphism matching.

use super::{
    graph_id_value, synthetic_doc_id, BTreeMap, BTreeSet, DocId, Edge, EdgeId, EdgePattern,
    GraphPattern, GraphPayload, GraphPostingList, GraphStore, GraphStoreError, GraphStoreResult,
    Payload, PostingEntry, PostingList, VertexId, DEFAULT_GRAPH_SCORE,
};

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

    pub fn execute<G: GraphStore>(&self, store: &G) -> GraphStoreResult<GraphPostingList> {
        if let Some(result) = self.try_execute_single_edge(store)? {
            return Ok(result);
        }
        if let Some(result) = self.try_execute_two_edge_path(store)? {
            return Ok(result);
        }

        let candidates = self.compute_candidates(store)?;

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
        self.backtrack(store, &candidates, &var_edges, &mut state)?;
        let mut matches = state.matches;

        if !negated_edges.is_empty() {
            let mut retained = Vec::with_capacity(matches.len());
            for assignment in matches {
                if Self::check_negated(store, self.graph, &negated_edges, &assignment)? {
                    retained.push(assignment);
                }
            }
            matches = retained;
        }

        let mut entries: Vec<PostingEntry> = Vec::with_capacity(matches.len());
        let mut graph_payloads: BTreeMap<DocId, GraphPayload> = BTreeMap::new();
        for (i, m) in matches.iter().enumerate() {
            let doc_id = synthetic_doc_id(i, "GMatch")?;
            let mut fields = BTreeMap::new();
            for (var, vid) in m {
                fields.insert(var.clone(), graph_id_value(*vid, "GMatch assignment")?);
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
            let subgraph_edges = Self::collect_match_edges(store, self.graph, &positive_edges, m)?;
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
        GraphPostingList::try_from_parts(
            PostingList::from_sorted_unchecked(entries),
            graph_payloads,
        )
        .map_err(Into::into)
    }

    fn try_execute_single_edge<G: GraphStore>(
        &self,
        store: &G,
    ) -> GraphStoreResult<Option<GraphPostingList>> {
        if self.pattern.vertex_patterns.len() != 2 || self.pattern.edge_patterns.len() != 1 {
            return Ok(None);
        }
        let edge_pattern = &self.pattern.edge_patterns[0];
        if edge_pattern.negated || edge_pattern.source_var == edge_pattern.target_var {
            return Ok(None);
        }
        let Some(source_pattern) = self.vertex_pattern(&edge_pattern.source_var) else {
            return Ok(None);
        };
        let Some(target_pattern) = self.vertex_pattern(&edge_pattern.target_var) else {
            return Ok(None);
        };
        let edge_list = self.edge_list_for_pattern(store, edge_pattern)?;

        let mut seen_assignments: BTreeSet<(VertexId, VertexId)> = BTreeSet::new();
        let mut entries: Vec<PostingEntry> = Vec::new();
        let mut graph_payloads: BTreeMap<DocId, GraphPayload> = BTreeMap::new();
        for edge in edge_list {
            if !edge_pattern.satisfies(&edge) {
                continue;
            }
            let source = store.get_vertex(edge.source_id).ok_or_else(|| {
                GraphStoreError::CorruptGraph(format!(
                    "edge {} references missing source vertex {}",
                    edge.edge_id, edge.source_id
                ))
            })?;
            if !source_pattern.satisfies(source) {
                continue;
            }
            let target = store.get_vertex(edge.target_id).ok_or_else(|| {
                GraphStoreError::CorruptGraph(format!(
                    "edge {} references missing target vertex {}",
                    edge.edge_id, edge.target_id
                ))
            })?;
            if !target_pattern.satisfies(target) {
                continue;
            }
            if !seen_assignments.insert((edge.source_id, edge.target_id)) {
                continue;
            }

            let doc_id = synthetic_doc_id(entries.len(), "single-edge GMatch")?;
            let mut fields = BTreeMap::new();
            fields.insert(
                edge_pattern.source_var.clone(),
                graph_id_value(edge.source_id, "GMatch source")?,
            );
            fields.insert(
                edge_pattern.target_var.clone(),
                graph_id_value(edge.target_id, "GMatch target")?,
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

        Ok(Some(GraphPostingList::try_from_parts(
            PostingList::from_sorted_unchecked(entries),
            graph_payloads,
        )?))
    }

    fn try_execute_two_edge_path<G: GraphStore>(
        &self,
        store: &G,
    ) -> GraphStoreResult<Option<GraphPostingList>> {
        if self.pattern.vertex_patterns.len() != 3 || self.pattern.edge_patterns.len() != 2 {
            return Ok(None);
        }
        let edge_patterns = &self.pattern.edge_patterns;
        if edge_patterns.iter().any(|edge| edge.negated) {
            return Ok(None);
        }
        let (first, second) = if edge_patterns[0].target_var == edge_patterns[1].source_var {
            (&edge_patterns[0], &edge_patterns[1])
        } else if edge_patterns[1].target_var == edge_patterns[0].source_var {
            (&edge_patterns[1], &edge_patterns[0])
        } else {
            return Ok(None);
        };
        if first.source_var == first.target_var
            || second.source_var == second.target_var
            || first.source_var == second.target_var
        {
            return Ok(None);
        }
        let Some(source_pattern) = self.vertex_pattern(&first.source_var) else {
            return Ok(None);
        };
        let Some(middle_pattern) = self.vertex_pattern(&first.target_var) else {
            return Ok(None);
        };
        let Some(target_pattern) = self.vertex_pattern(&second.target_var) else {
            return Ok(None);
        };

        let first_edges = self.edge_list_for_pattern(store, first)?;
        let mut seen_assignments: BTreeSet<(VertexId, VertexId, VertexId)> = BTreeSet::new();
        let mut entries: Vec<PostingEntry> = Vec::new();
        let mut graph_payloads: BTreeMap<DocId, GraphPayload> = BTreeMap::new();

        for first_edge in first_edges {
            if !first.satisfies(&first_edge) {
                continue;
            }
            let source = store.get_vertex(first_edge.source_id).ok_or_else(|| {
                GraphStoreError::CorruptGraph(format!(
                    "edge {} references missing source vertex {}",
                    first_edge.edge_id, first_edge.source_id
                ))
            })?;
            if !source_pattern.satisfies(source) {
                continue;
            }
            let middle = store.get_vertex(first_edge.target_id).ok_or_else(|| {
                GraphStoreError::CorruptGraph(format!(
                    "edge {} references missing middle vertex {}",
                    first_edge.edge_id, first_edge.target_id
                ))
            })?;
            if !middle_pattern.satisfies(middle) {
                continue;
            }

            for second_edge_id in store.out_edge_ids(first_edge.target_id, self.graph)? {
                let second_edge = store.get_edge(second_edge_id).ok_or_else(|| {
                    GraphStoreError::CorruptGraph(format!("missing path edge {second_edge_id}"))
                })?;
                if !second.satisfies(second_edge) {
                    continue;
                }
                if first_edge.source_id == second_edge.target_id
                    || first_edge.target_id == second_edge.target_id
                {
                    continue;
                }
                let target = store.get_vertex(second_edge.target_id).ok_or_else(|| {
                    GraphStoreError::CorruptGraph(format!(
                        "edge {} references missing target vertex {}",
                        second_edge.edge_id, second_edge.target_id
                    ))
                })?;
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
                )?;
            }
        }

        Ok(Some(GraphPostingList::try_from_parts(
            PostingList::from_sorted_unchecked(entries),
            graph_payloads,
        )?))
    }

    fn push_two_edge_path_match(
        &self,
        first: &EdgePattern,
        second: &EdgePattern,
        first_edge: &Edge,
        second_edge: &Edge,
        entries: &mut Vec<PostingEntry>,
        graph_payloads: &mut BTreeMap<DocId, GraphPayload>,
    ) -> GraphStoreResult<()> {
        let doc_id = synthetic_doc_id(entries.len(), "two-edge GMatch")?;
        let mut fields = BTreeMap::new();
        fields.insert(
            first.source_var.clone(),
            graph_id_value(first_edge.source_id, "GMatch source")?,
        );
        fields.insert(
            first.target_var.clone(),
            graph_id_value(first_edge.target_id, "GMatch middle")?,
        );
        fields.insert(
            second.target_var.clone(),
            graph_id_value(second_edge.target_id, "GMatch target")?,
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
        Ok(())
    }

    fn edge_list_for_pattern<G: GraphStore>(
        &self,
        store: &G,
        pattern: &EdgePattern,
    ) -> GraphStoreResult<Vec<Edge>> {
        let edge_ids = match pattern.label.as_deref() {
            Some(label) => store.edge_ids_by_label(label, self.graph)?,
            None => return store.edges_in_graph(self.graph),
        };
        edge_ids
            .into_iter()
            .map(|edge_id| {
                store.get_edge(edge_id).cloned().ok_or_else(|| {
                    GraphStoreError::CorruptGraph(format!("missing pattern edge {edge_id}"))
                })
            })
            .collect()
    }

    fn vertex_pattern(&self, variable: &str) -> Option<&crate::pattern::VertexPattern> {
        self.pattern
            .vertex_patterns
            .iter()
            .find(|pattern| pattern.variable == variable)
    }

    fn compute_candidates<G: GraphStore>(
        &self,
        store: &G,
    ) -> GraphStoreResult<BTreeMap<String, Vec<VertexId>>> {
        let mut candidates: BTreeMap<String, Vec<VertexId>> = BTreeMap::new();
        let graph_vids = store.vertex_ids_in_graph(self.graph)?;
        for vp in &self.pattern.vertex_patterns {
            let mut cands = Vec::new();
            for vid in &graph_vids {
                let vtx = store.get_vertex(*vid).ok_or_else(|| {
                    GraphStoreError::CorruptGraph(format!("missing candidate vertex {vid}"))
                })?;
                if vp.satisfies(vtx) {
                    cands.push(*vid);
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
                let mut new_src = Vec::new();
                for vid in candidates[src_var].iter().copied() {
                    if Self::has_edge_out(store, self.graph, vid, &tgt_set, ep)? {
                        new_src.push(vid);
                    }
                }
                if new_src.len() < candidates[src_var].len() {
                    candidates.insert(src_var.clone(), new_src);
                    changed = true;
                }
                let src_set: BTreeSet<VertexId> = candidates[src_var].iter().copied().collect();
                let mut new_tgt = Vec::new();
                for vid in candidates[tgt_var].iter().copied() {
                    if Self::has_edge_in(store, self.graph, vid, &src_set, ep)? {
                        new_tgt.push(vid);
                    }
                }
                if new_tgt.len() < candidates[tgt_var].len() {
                    candidates.insert(tgt_var.clone(), new_tgt);
                    changed = true;
                }
            }
        }
        Ok(candidates)
    }

    fn has_edge_out<G: GraphStore>(
        store: &G,
        graph: &str,
        src: VertexId,
        tgt_set: &BTreeSet<VertexId>,
        ep: &EdgePattern,
    ) -> GraphStoreResult<bool> {
        for eid in store.out_edge_ids(src, graph)? {
            let edge = store.get_edge(eid).ok_or_else(|| {
                GraphStoreError::CorruptGraph(format!("missing pattern edge {eid}"))
            })?;
            if !tgt_set.contains(&edge.target_id) {
                continue;
            }
            if ep.satisfies(edge) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn has_edge_in<G: GraphStore>(
        store: &G,
        graph: &str,
        tgt: VertexId,
        src_set: &BTreeSet<VertexId>,
        ep: &EdgePattern,
    ) -> GraphStoreResult<bool> {
        for eid in store.in_edge_ids(tgt, graph)? {
            let edge = store.get_edge(eid).ok_or_else(|| {
                GraphStoreError::CorruptGraph(format!("missing pattern edge {eid}"))
            })?;
            if !src_set.contains(&edge.source_id) {
                continue;
            }
            if ep.satisfies(edge) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn backtrack<G: GraphStore>(
        &self,
        store: &G,
        candidates: &BTreeMap<String, Vec<VertexId>>,
        var_edges: &BTreeMap<String, Vec<&EdgePattern>>,
        state: &mut BacktrackState,
    ) -> GraphStoreResult<()> {
        if state.unassigned.is_empty() {
            state.matches.push(state.assignment.clone());
            return Ok(());
        }
        let var = state
            .unassigned
            .iter()
            .min_by_key(|v| candidates.get(*v).map_or(usize::MAX, Vec::len))
            .cloned()
            .ok_or_else(|| {
                GraphStoreError::CorruptGraph(
                    "GMatch has no variable despite non-empty unassigned state".into(),
                )
            })?;
        let cands = candidates.get(&var).cloned().ok_or_else(|| {
            GraphStoreError::CorruptGraph(format!("missing candidate set for variable {var:?}"))
        })?;
        for vid in cands {
            if state.assigned_values.contains(&vid) {
                continue;
            }
            state.assignment.insert(var.clone(), vid);
            state.assigned_values.insert(vid);
            state.unassigned.remove(&var);
            if Self::validate_edges_for(store, self.graph, &var, var_edges, &state.assignment)? {
                self.backtrack(store, candidates, var_edges, state)?;
            }
            state.assignment.remove(&var);
            state.assigned_values.remove(&vid);
            state.unassigned.insert(var.clone());
        }
        Ok(())
    }

    fn validate_edges_for<G: GraphStore>(
        store: &G,
        graph: &str,
        var: &str,
        var_edges: &BTreeMap<String, Vec<&EdgePattern>>,
        assignment: &BTreeMap<String, VertexId>,
    ) -> GraphStoreResult<bool> {
        let Some(edges) = var_edges.get(var) else {
            return Ok(true);
        };
        for ep in edges {
            let (Some(src_id), Some(tgt_id)) = (
                assignment.get(&ep.source_var).copied(),
                assignment.get(&ep.target_var).copied(),
            ) else {
                continue;
            };
            let mut found = false;
            for eid in store.out_edge_ids(src_id, graph)? {
                let edge = store.get_edge(eid).ok_or_else(|| {
                    GraphStoreError::CorruptGraph(format!("missing pattern edge {eid}"))
                })?;
                if edge.target_id != tgt_id {
                    continue;
                }
                if ep.satisfies(edge) {
                    found = true;
                    break;
                }
            }
            if !found {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn check_negated<G: GraphStore>(
        store: &G,
        graph: &str,
        negated: &[&EdgePattern],
        assignment: &BTreeMap<String, VertexId>,
    ) -> GraphStoreResult<bool> {
        for ep in negated {
            let (Some(src_id), Some(tgt_id)) = (
                assignment.get(&ep.source_var).copied(),
                assignment.get(&ep.target_var).copied(),
            ) else {
                continue;
            };
            for eid in store.out_edge_ids(src_id, graph)? {
                let edge = store.get_edge(eid).ok_or_else(|| {
                    GraphStoreError::CorruptGraph(format!("missing pattern edge {eid}"))
                })?;
                if edge.target_id != tgt_id {
                    continue;
                }
                if ep.satisfies(edge) {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    fn collect_match_edges<G: GraphStore>(
        store: &G,
        graph: &str,
        positive: &[&EdgePattern],
        assignment: &BTreeMap<String, VertexId>,
    ) -> GraphStoreResult<Vec<EdgeId>> {
        let mut edges: BTreeSet<EdgeId> = BTreeSet::new();
        for ep in positive {
            let (Some(src_id), Some(tgt_id)) = (
                assignment.get(&ep.source_var).copied(),
                assignment.get(&ep.target_var).copied(),
            ) else {
                continue;
            };
            for eid in store.out_edge_ids(src_id, graph)? {
                let edge = store.get_edge(eid).ok_or_else(|| {
                    GraphStoreError::CorruptGraph(format!("missing pattern edge {eid}"))
                })?;
                if edge.target_id == tgt_id && ep.satisfies(edge) {
                    edges.insert(eid);
                    break;
                }
            }
        }
        Ok(edges.into_iter().collect())
    }
}

// -------------------------------------------------------------------------
// VertexAggregation
// -------------------------------------------------------------------------
