//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! MATCH expansion, vertex and relationship binding, and predicates.

use super::{
    agtype, pattern_variables, strict_bool, BTreeSet, Binding, BindingRow, CypherError,
    CypherExecutor, CypherExpr, Direction, Edge, EdgeId, GraphStore, MatchClause, MatchState,
    NodePattern, PathElement, PathPattern, RelDirection, RelPattern, ResultRow, Value, Vertex,
    VertexId,
};

impl<G: GraphStore> CypherExecutor<'_, G> {
    pub(crate) fn exec_match(
        &self,
        clause: &MatchClause,
        bindings: &[BindingRow],
    ) -> Result<Vec<BindingRow>, CypherError> {
        // Multiple comma-separated patterns combine as a cross-product:
        // each pattern further constrains the binding rows from the
        // previous pattern, sharing variable names where they appear.
        let mut next: Vec<BindingRow> = Vec::new();
        for row in bindings {
            let mut current: Vec<BindingRow> = vec![row.clone()];
            for pattern in &clause.patterns {
                let mut extended: Vec<BindingRow> = Vec::new();
                for partial in &current {
                    let mut matches = self.match_path_pattern(pattern, partial)?;
                    extended.append(&mut matches);
                }
                current = extended;
                if current.is_empty() {
                    break;
                }
            }
            if let Some(filter) = &clause.r#where {
                let mut filtered = Vec::with_capacity(current.len());
                for candidate in current {
                    if self.eval_predicate(filter, &candidate)? {
                        filtered.push(candidate);
                    }
                }
                current = filtered;
            }
            if current.is_empty() && clause.optional {
                // OPTIONAL MATCH pads unmatched rows with explicit null
                // bindings for every variable the patterns declare.
                let mut padded = row.clone();
                for var in pattern_variables(&clause.patterns) {
                    padded.entry(var).or_insert(Binding::Value(Value::Null));
                }
                next.push(padded);
            } else {
                next.extend(current);
            }
        }
        Ok(next)
    }

    pub(crate) fn match_path_pattern(
        &self,
        pattern: &PathPattern,
        seed: &BindingRow,
    ) -> Result<Vec<BindingRow>, CypherError> {
        let mut frontier: Vec<MatchState> = vec![MatchState {
            row: seed.clone(),
            position: None,
            trail: Vec::new(),
        }];
        let mut idx = 0;
        while idx < pattern.elements.len() {
            match &pattern.elements[idx] {
                PathElement::Node(np) => {
                    frontier = self.bind_node(np, &frontier)?;
                    idx += 1;
                }
                PathElement::Rel(rp) => {
                    let Some(PathElement::Node(next_node)) = pattern.elements.get(idx + 1) else {
                        return Err(CypherError::Unsupported("path must end on a node".into()));
                    };
                    frontier = self.traverse_rel(rp, next_node, frontier)?;
                    idx += 2;
                }
            }
        }
        let mut out = Vec::with_capacity(frontier.len());
        for state in frontier {
            let mut row = state.row;
            if let Some(path_var) = &pattern.variable {
                row.insert(
                    path_var.clone(),
                    Binding::Value(agtype::path_to_value(state.trail)),
                );
            }
            out.push(row);
        }
        Ok(out)
    }

    pub(super) fn bind_node(
        &self,
        np: &NodePattern,
        states: &[MatchState],
    ) -> Result<Vec<MatchState>, CypherError> {
        // Candidate vertex set: by label if specified, else everything in the graph.
        let candidate_ids: Vec<VertexId> = if let Some(label) = np.labels.first() {
            self.store.vertex_ids_by_label(label, self.graph)?
        } else {
            self.store
                .vertex_ids_in_graph(self.graph)?
                .into_iter()
                .collect()
        };

        let mut out = Vec::new();
        for state in states {
            // A variable already bound to a vertex pins the candidate.
            if let Some(var) = &np.variable {
                if let Some(Binding::Vertex(prev)) = state.row.get(var) {
                    let vertex = prev.clone();
                    if self.node_matches(np, &vertex, &state.row)? {
                        let mut new_state = state.clone();
                        new_state.trail.push(agtype::vertex_to_value(&vertex)?);
                        new_state.position = Some(vertex);
                        out.push(new_state);
                    }
                    continue;
                }
                if state.row.contains_key(var)
                    && !matches!(state.row.get(var), Some(Binding::Vertex(_)))
                {
                    // Bound to a non-vertex - the pattern cannot match.
                    continue;
                }
            }
            for vid in &candidate_ids {
                let Some(vertex) = self.store.get_vertex(*vid).cloned() else {
                    continue;
                };
                if !self.node_matches(np, &vertex, &state.row)? {
                    continue;
                }
                let mut new_state = state.clone();
                new_state.trail.push(agtype::vertex_to_value(&vertex)?);
                if let Some(var) = &np.variable {
                    new_state
                        .row
                        .insert(var.clone(), Binding::Vertex(vertex.clone()));
                }
                new_state.position = Some(vertex);
                out.push(new_state);
            }
        }
        Ok(out)
    }

    /// Whether `vertex_id` is consistent with an already-bound pattern variable. An unbound (or
    /// absent) variable always matches; a variable already bound to a vertex must refer to that
    /// same vertex. This enforces openCypher semantics when a relationship traversal reaches an
    /// end node whose variable was bound earlier in the pattern (e.g. `MERGE (a)-[:R]->(b)` with
    /// `b` already bound), so distinct edges from one start node are not collapsed.
    pub(super) fn binding_allows_vertex(
        variable: Option<&String>,
        vertex_id: VertexId,
        row: &BindingRow,
    ) -> bool {
        let Some(var) = variable else {
            return true;
        };
        match row.get(var) {
            Some(Binding::Vertex(prev)) => prev.vertex_id == vertex_id,
            Some(_) => false,
            None => true,
        }
    }

    pub(super) fn node_matches(
        &self,
        np: &NodePattern,
        vertex: &Vertex,
        row: &BindingRow,
    ) -> Result<bool, CypherError> {
        for label in &np.labels {
            if &vertex.label != label {
                return Ok(false);
            }
        }
        if let Some(props) = &np.properties {
            for (key, expr) in props {
                let want = self.eval(expr, row)?;
                let got = vertex.properties.get(key).cloned().unwrap_or(Value::Null);
                if got == Value::Null || !agtype::eq(&got, &want) {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    pub(super) fn rel_matches(
        &self,
        rp: &RelPattern,
        edge: &Edge,
        row: &BindingRow,
    ) -> Result<bool, CypherError> {
        if !rp.types.is_empty() && !rp.types.contains(&edge.label) {
            return Ok(false);
        }
        if let Some(props) = &rp.properties {
            for (key, expr) in props {
                let want = self.eval(expr, row)?;
                let got = edge.properties.get(key).cloned().unwrap_or(Value::Null);
                if got == Value::Null || !agtype::eq(&got, &want) {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    pub(super) fn traverse_rel(
        &self,
        rp: &RelPattern,
        next: &NodePattern,
        states: Vec<MatchState>,
    ) -> Result<Vec<MatchState>, CypherError> {
        let direction = match rp.direction {
            RelDirection::Right => Direction::Out,
            RelDirection::Left => Direction::In,
            RelDirection::Both => Direction::Both,
        };
        let is_var_length = rp.min_hops.is_some() || rp.max_hops.is_some();
        let mut out = Vec::new();
        for state in states {
            let Some(start) = state.position.clone() else {
                continue;
            };
            if is_var_length {
                let min_hops = rp.min_hops.unwrap_or(1);
                let max_hops = rp.max_hops.unwrap_or(min_hops.max(1));
                // (reached vertex, edges so far, trail extension)
                let mut buffer: Vec<(VertexId, Vec<Edge>, Vec<Value>)> =
                    vec![(start.vertex_id, Vec::new(), Vec::new())];
                let mut all_paths: Vec<(VertexId, Vec<Edge>, Vec<Value>)> = Vec::new();
                if min_hops == 0 {
                    all_paths.push((start.vertex_id, Vec::new(), Vec::new()));
                }
                for hop in 1..=max_hops {
                    let mut next_buffer = Vec::new();
                    for (vertex_id, edges_so_far, trail_so_far) in &buffer {
                        for edge in self.outgoing_edges(*vertex_id, direction)? {
                            if !self.rel_matches(rp, &edge, &state.row)? {
                                continue;
                            }
                            let neighbor = if edge.source_id == *vertex_id {
                                edge.target_id
                            } else {
                                edge.source_id
                            };
                            let Some(neighbor_vertex) = self.store.get_vertex(neighbor) else {
                                continue;
                            };
                            let mut new_edges = edges_so_far.clone();
                            new_edges.push(edge.clone());
                            let mut new_trail = trail_so_far.clone();
                            new_trail.push(agtype::edge_to_value(&edge)?);
                            new_trail.push(agtype::vertex_to_value(neighbor_vertex)?);
                            if hop >= min_hops {
                                all_paths.push((neighbor, new_edges.clone(), new_trail.clone()));
                            }
                            next_buffer.push((neighbor, new_edges, new_trail));
                        }
                    }
                    buffer = next_buffer;
                    if buffer.is_empty() {
                        break;
                    }
                }
                for (end_id, edges, mut trail_ext) in all_paths {
                    let Some(end_vertex) = self.store.get_vertex(end_id).cloned() else {
                        continue;
                    };
                    // The trail extension already ends with the reached
                    // vertex except for zero-hop paths.
                    if trail_ext.is_empty() {
                        trail_ext.push(agtype::vertex_to_value(&end_vertex)?);
                    }
                    self.push_reached_vertex(
                        &mut out,
                        rp,
                        next,
                        &state,
                        Binding::EdgeList(edges),
                        end_vertex,
                        trail_ext,
                    )?;
                }
            } else {
                for edge in self.outgoing_edges(start.vertex_id, direction)? {
                    if !self.rel_matches(rp, &edge, &state.row)? {
                        continue;
                    }
                    let neighbor_id = if edge.source_id == start.vertex_id {
                        edge.target_id
                    } else {
                        edge.source_id
                    };
                    let Some(end_vertex) = self.store.get_vertex(neighbor_id).cloned() else {
                        continue;
                    };
                    let trail_ext = vec![
                        agtype::edge_to_value(&edge)?,
                        agtype::vertex_to_value(&end_vertex)?,
                    ];
                    self.push_reached_vertex(
                        &mut out,
                        rp,
                        next,
                        &state,
                        Binding::Edge(edge),
                        end_vertex,
                        trail_ext,
                    )?;
                }
            }
        }
        Ok(out)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn push_reached_vertex(
        &self,
        out: &mut Vec<MatchState>,
        rp: &RelPattern,
        next: &NodePattern,
        state: &MatchState,
        edge_binding: Binding,
        end_vertex: Vertex,
        trail_ext: Vec<Value>,
    ) -> Result<(), CypherError> {
        if !self.node_matches(next, &end_vertex, &state.row)?
            || !Self::binding_allows_vertex(
                next.variable.as_ref(),
                end_vertex.vertex_id,
                &state.row,
            )
        {
            return Ok(());
        }
        let mut new_state = state.clone();
        new_state.trail.extend(trail_ext);
        if let Some(var) = &rp.variable {
            new_state.row.insert(var.clone(), edge_binding);
        }
        if let Some(var) = &next.variable {
            new_state
                .row
                .insert(var.clone(), Binding::Vertex(end_vertex.clone()));
        }
        new_state.position = Some(end_vertex);
        out.push(new_state);
        Ok(())
    }

    pub(super) fn outgoing_edges(
        &self,
        vertex_id: VertexId,
        direction: Direction,
    ) -> Result<Vec<Edge>, CypherError> {
        let mut ids: BTreeSet<EdgeId> = BTreeSet::new();
        match direction {
            Direction::Out => {
                ids.extend(self.store.out_edge_ids(vertex_id, self.graph)?);
            }
            Direction::In => {
                ids.extend(self.store.in_edge_ids(vertex_id, self.graph)?);
            }
            Direction::Both => {
                ids.extend(self.store.out_edge_ids(vertex_id, self.graph)?);
                ids.extend(self.store.in_edge_ids(vertex_id, self.graph)?);
            }
        }
        ids.into_iter()
            .map(|eid| {
                self.store.get_edge(eid).cloned().ok_or_else(|| {
                    CypherError::Storage(format!("graph adjacency references missing edge {eid}"))
                })
            })
            .collect()
    }

    pub(super) fn eval_predicate(
        &self,
        expr: &CypherExpr,
        row: &BindingRow,
    ) -> Result<bool, CypherError> {
        Ok(strict_bool(&self.eval(expr, row)?)? == Some(true))
    }

    pub(crate) fn where_passes(
        &self,
        expr: &CypherExpr,
        row: &ResultRow,
    ) -> Result<bool, CypherError> {
        let bindings: BindingRow = row
            .iter()
            .map(|(k, v)| (k.clone(), Binding::Value(v.clone())))
            .collect();
        self.eval_predicate(expr, &bindings)
    }

    // ------------------------------------------------------------------
    // RETURN / WITH
    // ------------------------------------------------------------------
}
