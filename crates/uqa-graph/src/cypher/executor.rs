//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read-only Cypher executor: walks a `CypherQuery` AST and lowers it
//! onto graph operators against a [`GraphStore`].
//!
//! Semantics follow Apache AGE 1.6.0 (verified against a live
//! container): agtype total ordering for `ORDER BY` and comparisons,
//! three-valued boolean logic with strict boolean inputs, C-style
//! integer division / modulo (`n % 0` returns `n`, matching AGE),
//! float `^` power, end-exclusive list slices, end-inclusive
//! `range()`, byte-length `size()` on strings, unanchored `=~`, and
//! graph entities that render as `::vertex` / `::edge` / `::path`.
//!
//! Supported clauses: `MATCH` (node, 1-hop rel, variable-length rel,
//! path variables), `OPTIONAL MATCH`, `WHERE`, `RETURN` (with
//! `DISTINCT`, `ORDER BY`, `SKIP`, `LIMIT`), and `WITH`. Mutation
//! clauses live in [`crate::cypher::writer::CypherWriter`].

use std::collections::{BTreeMap, BTreeSet};

use uqa_core::{Edge, EdgeId, Value, Vertex, VertexId};

use crate::agtype;
use crate::cypher::ast::{
    BinaryOp, CaseExpr, CypherClause, CypherExpr, CypherQuery, FunctionCall, InList, IsNotNull,
    IsNull, ListComprehension, ListIndex, ListLiteral, ListSlice, Literal, MapLiteral, MatchClause,
    NodePattern, OrderByItem, Parameter, PathElement, PathPattern, PropertyAccess, RelDirection,
    RelPattern, ReturnItem, UnaryOp, Variable,
};
use crate::store::GraphStore;
use crate::types::Direction;

/// One row in the binding table threaded through the clause pipeline.
/// Variables can resolve to vertex / edge / arbitrary value bindings.
#[derive(Debug, Clone)]
pub enum Binding {
    Vertex(Vertex),
    Edge(Edge),
    Value(Value),
    /// Variable-length relationship binding: ordered list of edges.
    EdgeList(Vec<Edge>),
}

impl Binding {
    fn property(&self, key: &str) -> Value {
        match self {
            Binding::Vertex(v) => v.properties.get(key).cloned().unwrap_or(Value::Null),
            Binding::Edge(e) => e.properties.get(key).cloned().unwrap_or(Value::Null),
            Binding::Value(v) => value_property(v, key).unwrap_or(Value::Null),
            Binding::EdgeList(_) => Value::Null,
        }
    }

    fn to_value(&self) -> Result<Value, CypherError> {
        match self {
            Binding::Vertex(v) => agtype::vertex_to_value(v).map_err(Into::into),
            Binding::Edge(e) => agtype::edge_to_value(e).map_err(Into::into),
            Binding::Value(v) => Ok(v.clone()),
            Binding::EdgeList(edges) => Ok(Value::List(
                edges
                    .iter()
                    .map(agtype::edge_to_value)
                    .collect::<Result<_, _>>()?,
            )),
        }
    }
}

/// Property lookup on an evaluated value (map, entity envelope, or
/// null). `None` signals "not addressable" so callers can raise AGE's
/// `scalar object must be a vertex or edge` error.
fn value_property(value: &Value, key: &str) -> Option<Value> {
    if let Some(props) = agtype::entity_properties(value) {
        return Some(props.get(key).cloned().unwrap_or(Value::Null));
    }
    match value {
        Value::Map(map) => Some(map.get(key).cloned().unwrap_or(Value::Null)),
        Value::Null => Some(Value::Null),
        _ => None,
    }
}

/// A row in the binding table: variable name -> bound value.
pub type BindingRow = BTreeMap<String, Binding>;

/// Result row produced by RETURN / WITH (column name -> value).
pub type ResultRow = BTreeMap<String, Value>;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum CypherError {
    #[error("undefined variable {0:?}")]
    UndefinedVariable(String),
    #[error("undefined parameter {0:?}")]
    UndefinedParameter(String),
    #[error("unsupported clause: {0}")]
    Unsupported(String),
    #[error("{0}")]
    TypeError(String),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("storage error: {0}")]
    Storage(String),
}

impl From<crate::cypher::parser::ParseError> for CypherError {
    fn from(err: crate::cypher::parser::ParseError) -> Self {
        CypherError::Parse(err.to_string())
    }
}

impl From<agtype::AgtypeConversionError> for CypherError {
    fn from(err: agtype::AgtypeConversionError) -> Self {
        CypherError::Storage(err.to_string())
    }
}

impl From<crate::store::GraphStoreError> for CypherError {
    fn from(err: crate::store::GraphStoreError) -> Self {
        CypherError::Storage(err.to_string())
    }
}

fn boolean_cast_error(value: &Value) -> CypherError {
    CypherError::TypeError(format!(
        "cannot cast agtype {} to type boolean",
        agtype::agtype_type_name(value)
    ))
}

/// Strict boolean coercion (AGE): booleans pass through, null is
/// three-valued unknown, anything else raises a cast error.
fn strict_bool(value: &Value) -> Result<Option<bool>, CypherError> {
    match value {
        Value::Bool(b) => Ok(Some(*b)),
        Value::Null => Ok(None),
        other => Err(boolean_cast_error(other)),
    }
}

const MAX_EXACT_F64_INTEGER: i64 = 9_007_199_254_740_992;

fn usize_to_i64(value: usize, context: &str) -> Result<i64, CypherError> {
    i64::try_from(value).map_err(|_| {
        CypherError::TypeError(format!(
            "{context} {value} exceeds the agtype integer range"
        ))
    })
}

fn nonnegative_i64_to_usize(value: i64, context: &str) -> Result<usize, CypherError> {
    if value < 0 {
        return Err(CypherError::TypeError(format!(
            "{context} must not be negative, got {value}"
        )));
    }
    usize::try_from(value).map_err(|_| {
        CypherError::TypeError(format!(
            "{context} {value} exceeds the platform index range"
        ))
    })
}

fn nonnegative_i64_to_u64(value: i64, context: &str) -> Result<u64, CypherError> {
    u64::try_from(value)
        .map_err(|_| CypherError::TypeError(format!("{context} must not be negative, got {value}")))
}

fn exact_i64_to_f64(value: i64, context: &str) -> Result<f64, CypherError> {
    if (-MAX_EXACT_F64_INTEGER..=MAX_EXACT_F64_INTEGER).contains(&value) {
        Ok(value as f64)
    } else {
        Err(CypherError::TypeError(format!(
            "{context} {value} cannot be represented exactly as a float"
        )))
    }
}

fn trunc_f64_to_i64(value: f64, context: &str) -> Result<i64, CypherError> {
    if !value.is_finite() {
        return Err(CypherError::TypeError(format!(
            "{context} must be finite, got {value}"
        )));
    }
    let truncated = value.trunc();
    if !(-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&truncated) {
        return Err(CypherError::TypeError(format!(
            "{context} {value} is outside the agtype integer range"
        )));
    }
    Ok(truncated as i64)
}

/// Read-only execution context.
pub struct CypherExecutor<'a, G: GraphStore> {
    pub store: &'a G,
    pub graph: &'a str,
    pub params: BTreeMap<String, Value>,
}

/// Intermediate state while a path pattern binds: the row so far, the
/// vertex the pattern currently stands on (anonymous nodes have no
/// variable to look up), and the ordered vertex / edge trail (for
/// `p = (...)` path variables).
#[derive(Debug, Clone)]
struct MatchState {
    row: BindingRow,
    position: Option<Vertex>,
    trail: Vec<Value>,
}

impl<'a, G: GraphStore> CypherExecutor<'a, G> {
    pub fn new(store: &'a G, graph: &'a str) -> Self {
        Self {
            store,
            graph,
            params: BTreeMap::new(),
        }
    }

    pub fn with_params(mut self, params: BTreeMap<String, Value>) -> Self {
        self.params = params;
        self
    }

    pub fn execute(
        &self,
        query: &CypherQuery,
    ) -> Result<(Vec<String>, Vec<ResultRow>), CypherError> {
        let mut bindings: Vec<BindingRow> = vec![BTreeMap::new()];
        let mut columns: Vec<String> = Vec::new();
        let mut rows: Vec<ResultRow> = Vec::new();
        for clause in &query.clauses {
            match clause {
                CypherClause::Match(m) => {
                    bindings = self.exec_match(m, &bindings)?;
                }
                CypherClause::With(w) => {
                    let (cols, projected) = self.exec_return_like(
                        &w.items,
                        w.distinct,
                        w.order_by.as_deref(),
                        w.skip.as_ref(),
                        w.limit.as_ref(),
                        &bindings,
                    )?;
                    let mut next = Vec::with_capacity(projected.len());
                    for row in projected {
                        if let Some(filter) = &w.r#where {
                            if !self.where_passes(filter, &row)? {
                                continue;
                            }
                        }
                        next.push(Self::row_to_bindings(&cols, &row));
                    }
                    bindings = next;
                }
                CypherClause::Return(r) => {
                    let (cols, ret_rows) = self.exec_return_like(
                        &r.items,
                        r.distinct,
                        r.order_by.as_deref(),
                        r.skip.as_ref(),
                        r.limit.as_ref(),
                        &bindings,
                    )?;
                    columns = cols;
                    rows = ret_rows;
                }
                CypherClause::Create(_)
                | CypherClause::Merge(_)
                | CypherClause::Set(_)
                | CypherClause::Delete(_)
                | CypherClause::Unwind(_) => {
                    return Err(CypherError::Unsupported(format!("{clause:?}")));
                }
            }
        }
        Ok((columns, rows))
    }

    pub(crate) fn row_to_bindings(cols: &[String], row: &ResultRow) -> BindingRow {
        let mut out = BindingRow::new();
        for col in cols {
            if let Some(v) = row.get(col) {
                out.insert(col.clone(), Binding::Value(v.clone()));
            }
        }
        out
    }

    // ------------------------------------------------------------------
    // MATCH
    // ------------------------------------------------------------------

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

    fn bind_node(
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
    fn binding_allows_vertex(
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

    fn node_matches(
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

    fn rel_matches(
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

    fn traverse_rel(
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
    fn push_reached_vertex(
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

    fn outgoing_edges(
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

    fn eval_predicate(&self, expr: &CypherExpr, row: &BindingRow) -> Result<bool, CypherError> {
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

    pub(crate) fn exec_return_like(
        &self,
        items: &[ReturnItem],
        distinct: bool,
        order_by: Option<&[OrderByItem]>,
        skip: Option<&CypherExpr>,
        limit: Option<&CypherExpr>,
        bindings: &[BindingRow],
    ) -> Result<(Vec<String>, Vec<ResultRow>), CypherError> {
        let mut columns: Vec<String> = items
            .iter()
            .enumerate()
            .map(|(i, item)| return_label(item, i))
            .collect();
        // Result rows are keyed by column name; repeated unaliased
        // labels (e.g. `RETURN size(a), size(b)`) must stay distinct
        // so later columns do not clobber earlier ones.
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for (i, column) in columns.iter_mut().enumerate() {
            if !seen.insert(column.clone()) {
                let mut candidate = format!("{column}_{i}");
                while !seen.insert(candidate.clone()) {
                    candidate.push('_');
                }
                *column = candidate;
            }
        }

        let has_aggregate = items.iter().any(|i| is_aggregate(&i.expr));
        let mut rows: Vec<ResultRow> = if has_aggregate {
            let mut out = self.aggregate_return(items, &columns, bindings)?;
            if let Some(order) = order_by {
                self.sort_result_rows(&mut out, order)?;
            }
            out
        } else {
            // For non-aggregate flows, ORDER BY / SKIP / LIMIT operate
            // on the *binding rows* so the ordering expression can
            // still reference variables that won't survive projection
            // (e.g. `RETURN n.name ORDER BY n.age`).
            let mut binding_rows: Vec<BindingRow> = bindings.to_vec();
            if let Some(order) = order_by {
                self.sort_binding_rows(&mut binding_rows, order, items, &columns)?;
            }
            if let Some(skip_expr) = skip {
                let n =
                    nonnegative_i64_to_usize(self.eval_int(skip_expr, &BTreeMap::new())?, "SKIP")?;
                if n >= binding_rows.len() {
                    binding_rows.clear();
                } else {
                    binding_rows.drain(0..n);
                }
            }
            if let Some(limit_expr) = limit {
                let n = nonnegative_i64_to_usize(
                    self.eval_int(limit_expr, &BTreeMap::new())?,
                    "LIMIT",
                )?;
                binding_rows.truncate(n);
            }
            let mut out = Vec::with_capacity(binding_rows.len());
            for row in &binding_rows {
                let mut result = ResultRow::new();
                for (i, item) in items.iter().enumerate() {
                    let value = self.eval(&item.expr, row)?;
                    result.insert(columns[i].clone(), value);
                }
                out.push(result);
            }
            out
        };

        if distinct {
            let mut seen: BTreeSet<Vec<Value>> = BTreeSet::new();
            rows.retain(|row| {
                let key: Vec<Value> = columns
                    .iter()
                    .map(|c| row.get(c).cloned().unwrap_or(Value::Null))
                    .collect();
                seen.insert(key)
            });
        }

        if has_aggregate {
            if let Some(skip_expr) = skip {
                let n =
                    nonnegative_i64_to_usize(self.eval_int(skip_expr, &BTreeMap::new())?, "SKIP")?;
                if n >= rows.len() {
                    rows.clear();
                } else {
                    rows.drain(0..n);
                }
            }
            if let Some(limit_expr) = limit {
                let n = nonnegative_i64_to_usize(
                    self.eval_int(limit_expr, &BTreeMap::new())?,
                    "LIMIT",
                )?;
                rows.truncate(n);
            }
        }

        Ok((columns, rows))
    }

    fn aggregate_return(
        &self,
        items: &[ReturnItem],
        columns: &[String],
        bindings: &[BindingRow],
    ) -> Result<Vec<ResultRow>, CypherError> {
        // Group by the non-aggregate items.
        let group_by_idx: Vec<usize> = items
            .iter()
            .enumerate()
            .filter(|(_, it)| !is_aggregate(&it.expr))
            .map(|(i, _)| i)
            .collect();

        let mut groups: BTreeMap<Vec<Value>, Vec<BindingRow>> = BTreeMap::new();
        if group_by_idx.is_empty() && bindings.is_empty() {
            // Aggregates over zero rows still produce one output row
            // (count(*) = 0, sum(...) = null, collect(...) = []).
            groups.insert(Vec::new(), Vec::new());
        }
        for row in bindings {
            let key: Vec<Value> = group_by_idx
                .iter()
                .map(|&i| self.eval(&items[i].expr, row))
                .collect::<Result<_, _>>()?;
            groups.entry(key).or_default().push(row.clone());
        }

        let mut out = Vec::with_capacity(groups.len());
        for (group_key, members) in groups {
            let mut result = ResultRow::new();
            // Non-aggregates: use group key.
            for (out_pos, &i) in group_by_idx.iter().enumerate() {
                result.insert(columns[i].clone(), group_key[out_pos].clone());
            }
            for (i, item) in items.iter().enumerate() {
                if !is_aggregate(&item.expr) {
                    continue;
                }
                let value = self.eval_aggregate(&item.expr, &members)?;
                result.insert(columns[i].clone(), value);
            }
            out.push(result);
        }
        Ok(out)
    }

    fn eval_aggregate(
        &self,
        expr: &CypherExpr,
        members: &[BindingRow],
    ) -> Result<Value, CypherError> {
        let CypherExpr::FunctionCall(fc) = expr else {
            return Err(CypherError::Unsupported(format!(
                "non-function aggregate: {expr:?}"
            )));
        };
        let name = fc.name.to_lowercase();
        if name == "count" {
            let is_star = fc.args.is_empty()
                || fc
                    .args
                    .iter()
                    .any(|a| matches!(a, CypherExpr::Variable(v) if v.name == "*"));
            if is_star {
                return Ok(Value::Int(usize_to_i64(
                    members.len(),
                    "aggregate row count",
                )?));
            }
            let mut count = 0i64;
            let mut seen: BTreeSet<Value> = BTreeSet::new();
            for row in members {
                let v = self.eval(&fc.args[0], row)?;
                if v == Value::Null {
                    continue;
                }
                if fc.distinct {
                    if seen.insert(v) {
                        count = count.checked_add(1).ok_or_else(|| {
                            CypherError::TypeError("count() result exceeds bigint range".into())
                        })?;
                    }
                } else {
                    count = count.checked_add(1).ok_or_else(|| {
                        CypherError::TypeError("count() result exceeds bigint range".into())
                    })?;
                }
            }
            return Ok(Value::Int(count));
        }

        // Evaluate the argument across members, skipping nulls, with
        // optional DISTINCT dedup.
        let mut values: Vec<Value> = Vec::new();
        let mut seen: BTreeSet<Value> = BTreeSet::new();
        for row in members {
            let v = self.eval(&fc.args[0], row)?;
            if v == Value::Null {
                continue;
            }
            if fc.distinct && !seen.insert(v.clone()) {
                continue;
            }
            values.push(v);
        }

        match name.as_str() {
            "collect" => Ok(Value::List(values)),
            "min" | "max" => Ok(aggregate_extreme(&values, name == "min")),
            "sum" => aggregate_sum(&values),
            "avg" => aggregate_avg(&values),
            other => Err(CypherError::Unsupported(format!("aggregate {other}"))),
        }
    }

    fn sort_result_rows(
        &self,
        rows: &mut [ResultRow],
        order: &[OrderByItem],
    ) -> Result<(), CypherError> {
        let mut keyed: Vec<(Vec<Value>, ResultRow)> = rows
            .iter()
            .cloned()
            .map(|row| -> Result<_, CypherError> {
                let bindings: BindingRow = row
                    .iter()
                    .map(|(k, v)| (k.clone(), Binding::Value(v.clone())))
                    .collect();
                let key: Vec<Value> = order
                    .iter()
                    .map(|o| self.eval(&o.expr, &bindings))
                    .collect::<Result<_, _>>()?;
                Ok((key, row))
            })
            .collect::<Result<Vec<_>, _>>()?;
        sort_keyed(&mut keyed, order);
        for (i, (_, row)) in keyed.into_iter().enumerate() {
            rows[i] = row;
        }
        Ok(())
    }

    fn sort_binding_rows(
        &self,
        rows: &mut [BindingRow],
        order: &[OrderByItem],
        items: &[ReturnItem],
        columns: &[String],
    ) -> Result<(), CypherError> {
        let mut keyed: Vec<(Vec<Value>, BindingRow)> = rows
            .iter()
            .cloned()
            .map(|row| -> Result<_, CypherError> {
                // Overlay projected aliases on top of the source bindings
                // so ORDER BY can reference either.
                let mut overlay = row.clone();
                for (i, item) in items.iter().enumerate() {
                    let v = self.eval(&item.expr, &row)?;
                    overlay.insert(columns[i].clone(), Binding::Value(v));
                }
                let key: Vec<Value> = order
                    .iter()
                    .map(|o| self.eval(&o.expr, &overlay))
                    .collect::<Result<_, _>>()?;
                Ok((key, row))
            })
            .collect::<Result<Vec<_>, _>>()?;
        sort_keyed(&mut keyed, order);
        for (i, (_, row)) in keyed.into_iter().enumerate() {
            rows[i] = row;
        }
        Ok(())
    }

    fn eval_int(&self, expr: &CypherExpr, row: &BindingRow) -> Result<i64, CypherError> {
        match self.eval(expr, row)? {
            Value::Int(n) => Ok(n),
            Value::Float(f) => trunc_f64_to_i64(f, "integer expression"),
            other => Err(CypherError::TypeError(format!(
                "expected integer, got {other:?}"
            ))),
        }
    }

    // ------------------------------------------------------------------
    // Expression evaluation
    // ------------------------------------------------------------------

    pub(crate) fn eval(&self, expr: &CypherExpr, row: &BindingRow) -> Result<Value, CypherError> {
        match expr {
            CypherExpr::Literal(Literal { value }) => Ok(value.clone()),
            CypherExpr::Parameter(Parameter { name }) => self
                .params
                .get(name)
                .cloned()
                .ok_or_else(|| CypherError::UndefinedParameter(name.clone())),
            CypherExpr::Variable(Variable { name }) => match row.get(name) {
                Some(b) => b.to_value(),
                None => Err(CypherError::UndefinedVariable(name.clone())),
            },
            CypherExpr::PropertyAccess(PropertyAccess { variable, keys }) => {
                let Some(binding) = row.get(variable) else {
                    return Err(CypherError::UndefinedVariable(variable.clone()));
                };
                let mut value = match binding {
                    Binding::Vertex(_) | Binding::Edge(_) => binding.property(&keys[0]),
                    Binding::EdgeList(_) => {
                        return Err(CypherError::TypeError(
                            "scalar object must be a vertex or edge".into(),
                        ));
                    }
                    Binding::Value(v) => value_property(v, &keys[0]).ok_or_else(|| {
                        CypherError::TypeError("scalar object must be a vertex or edge".into())
                    })?,
                };
                for key in &keys[1..] {
                    value = value_property(&value, key).ok_or_else(|| {
                        CypherError::TypeError("scalar object must be a vertex or edge".into())
                    })?;
                }
                Ok(value)
            }
            CypherExpr::BinaryOp(b) => self.eval_binary(b, row),
            CypherExpr::UnaryOp(u) => self.eval_unary(u, row),
            CypherExpr::ListIndex(li) => self.eval_list_index(li, row),
            CypherExpr::ListSlice(ls) => self.eval_list_slice(ls, row),
            CypherExpr::ListLiteral(ll) => self.eval_list_literal(ll, row),
            CypherExpr::ListComprehension(lc) => self.eval_list_comprehension(lc, row),
            CypherExpr::InList(il) => self.eval_in_list(il, row),
            CypherExpr::IsNull(IsNull { expr }) => {
                Ok(Value::Bool(matches!(self.eval(expr, row)?, Value::Null)))
            }
            CypherExpr::IsNotNull(IsNotNull { expr }) => {
                Ok(Value::Bool(!matches!(self.eval(expr, row)?, Value::Null)))
            }
            CypherExpr::CaseExpr(c) => self.eval_case(c, row),
            CypherExpr::FunctionCall(fc) => self.eval_function(fc, row),
            CypherExpr::MapLiteral(ml) => self.eval_map_literal(ml, row),
            CypherExpr::ExistsPattern(pattern) => {
                let matches = self.match_path_pattern(pattern, row)?;
                Ok(Value::Bool(!matches.is_empty()))
            }
        }
    }

    fn eval_binary(&self, expr: &BinaryOp, row: &BindingRow) -> Result<Value, CypherError> {
        let lhs = self.eval(&expr.left, row)?;
        let rhs = self.eval(&expr.right, row)?;
        match expr.op.as_str() {
            "AND" => {
                let left_bool = strict_bool(&lhs)?;
                let right_bool = strict_bool(&rhs)?;
                Ok(match (left_bool, right_bool) {
                    (Some(false), _) | (_, Some(false)) => Value::Bool(false),
                    (Some(true), Some(true)) => Value::Bool(true),
                    _ => Value::Null,
                })
            }
            "OR" => {
                let left_bool = strict_bool(&lhs)?;
                let right_bool = strict_bool(&rhs)?;
                Ok(match (left_bool, right_bool) {
                    (Some(true), _) | (_, Some(true)) => Value::Bool(true),
                    (Some(false), Some(false)) => Value::Bool(false),
                    _ => Value::Null,
                })
            }
            "XOR" => {
                let left_bool = strict_bool(&lhs)?;
                let right_bool = strict_bool(&rhs)?;
                Ok(match (left_bool, right_bool) {
                    (Some(x), Some(y)) => Value::Bool(x ^ y),
                    _ => Value::Null,
                })
            }
            "=" => Ok(null_or_bool(&lhs, &rhs, agtype::eq(&lhs, &rhs))),
            "<>" => Ok(null_or_bool(&lhs, &rhs, !agtype::eq(&lhs, &rhs))),
            "<" => Ok(null_or_bool(
                &lhs,
                &rhs,
                agtype::cmp(&lhs, &rhs) == std::cmp::Ordering::Less,
            )),
            ">" => Ok(null_or_bool(
                &lhs,
                &rhs,
                agtype::cmp(&lhs, &rhs) == std::cmp::Ordering::Greater,
            )),
            "<=" => Ok(null_or_bool(
                &lhs,
                &rhs,
                agtype::cmp(&lhs, &rhs) != std::cmp::Ordering::Greater,
            )),
            ">=" => Ok(null_or_bool(
                &lhs,
                &rhs,
                agtype::cmp(&lhs, &rhs) != std::cmp::Ordering::Less,
            )),
            "+" => agtype_add(&lhs, &rhs),
            "-" => numeric_op(&lhs, &rhs, "agtype_sub", i64::wrapping_sub, |a, b| a - b),
            "*" => numeric_op(&lhs, &rhs, "agtype_mul", i64::wrapping_mul, |a, b| a * b),
            "/" => agtype_div(&lhs, &rhs),
            "%" => agtype_mod(&lhs, &rhs),
            "^" => agtype_pow(&lhs, &rhs),
            "STARTS WITH" => Ok(str_predicate(&lhs, &rhs, |a, b| a.starts_with(b))),
            "ENDS WITH" => Ok(str_predicate(&lhs, &rhs, |a, b| a.ends_with(b))),
            "CONTAINS" => Ok(str_predicate(&lhs, &rhs, |a, b| a.contains(b))),
            "=~" => regex_match(&lhs, &rhs),
            other => Err(CypherError::Unsupported(format!("binary op {other}"))),
        }
    }

    fn eval_unary(&self, u: &UnaryOp, row: &BindingRow) -> Result<Value, CypherError> {
        let operand = self.eval(&u.operand, row)?;
        match u.op.as_str() {
            "NOT" => Ok(match strict_bool(&operand)? {
                Some(b) => Value::Bool(!b),
                None => Value::Null,
            }),
            "-" => match operand {
                Value::Int(n) => Ok(Value::Int(n.wrapping_neg())),
                Value::Float(f) => Ok(Value::Float(-f)),
                Value::Null => Ok(Value::Null),
                _ => Err(CypherError::TypeError(
                    "Invalid input parameter type for agtype_neg".into(),
                )),
            },
            other => Err(CypherError::Unsupported(format!("unary op {other}"))),
        }
    }

    fn eval_list_literal(&self, ll: &ListLiteral, row: &BindingRow) -> Result<Value, CypherError> {
        let mut out = Vec::with_capacity(ll.elements.len());
        for element in &ll.elements {
            out.push(self.eval(element, row)?);
        }
        Ok(Value::List(out))
    }

    fn eval_map_literal(&self, ml: &MapLiteral, row: &BindingRow) -> Result<Value, CypherError> {
        let mut out = BTreeMap::new();
        for (key, expr) in &ml.pairs {
            out.insert(key.clone(), self.eval(expr, row)?);
        }
        Ok(Value::Map(out))
    }

    fn eval_list_comprehension(
        &self,
        lc: &ListComprehension,
        row: &BindingRow,
    ) -> Result<Value, CypherError> {
        let source = self.eval(&lc.list_expr, row)?;
        let items = match source {
            Value::List(items) => items,
            Value::Null => return Ok(Value::Null),
            other => {
                return Err(CypherError::TypeError(format!(
                    "list comprehension requires a list, got agtype {}",
                    agtype::agtype_type_name(&other)
                )));
            }
        };
        let mut out = Vec::new();
        for item in items {
            let mut scoped = row.clone();
            scoped.insert(lc.variable.clone(), Binding::Value(item.clone()));
            if let Some(filter) = &lc.filter {
                if !self.eval_predicate(filter, &scoped)? {
                    continue;
                }
            }
            match &lc.map_expr {
                Some(map_expr) => out.push(self.eval(map_expr, &scoped)?),
                None => out.push(item),
            }
        }
        Ok(Value::List(out))
    }

    fn eval_in_list(&self, il: &InList, row: &BindingRow) -> Result<Value, CypherError> {
        let needle = self.eval(&il.expr, row)?;
        let haystack = self.eval(&il.list_expr, row)?;
        let items = match haystack {
            Value::List(items) => items,
            Value::Null => return Ok(Value::Null),
            _ => {
                return Err(CypherError::TypeError("object of IN must be a list".into()));
            }
        };
        if items.is_empty() {
            return Ok(Value::Bool(false));
        }
        if needle == Value::Null {
            return Ok(Value::Null);
        }
        let mut saw_null = false;
        for item in &items {
            if *item == Value::Null {
                saw_null = true;
            } else if agtype::eq(item, &needle) {
                return Ok(Value::Bool(true));
            }
        }
        if saw_null {
            Ok(Value::Null)
        } else {
            Ok(Value::Bool(false))
        }
    }

    fn eval_list_index(&self, li: &ListIndex, row: &BindingRow) -> Result<Value, CypherError> {
        let target = self.eval(&li.expr, row)?;
        let index = self.eval(&li.index, row)?;
        if target == Value::Null || index == Value::Null {
            return Ok(Value::Null);
        }
        if let Some(props) = agtype::entity_properties(&target) {
            return match index {
                Value::Str(key) => Ok(props.get(&key).cloned().unwrap_or(Value::Null)),
                _ => Err(CypherError::TypeError(
                    "object index must resolve to a string value".into(),
                )),
            };
        }
        match (&target, &index) {
            (Value::List(items), Value::Int(n)) => {
                let idx = if *n < 0 {
                    let len = usize_to_i64(items.len(), "list length")?;
                    let adjusted = len.checked_add(*n).ok_or_else(|| {
                        CypherError::TypeError("list index arithmetic overflow".into())
                    })?;
                    if adjusted < 0 {
                        return Ok(Value::Null);
                    }
                    nonnegative_i64_to_usize(adjusted, "list index")?
                } else {
                    nonnegative_i64_to_usize(*n, "list index")?
                };
                Ok(items.get(idx).cloned().unwrap_or(Value::Null))
            }
            (Value::List(_), _) => Err(CypherError::TypeError(
                "array index must resolve to an integer value".into(),
            )),
            (Value::Map(m), Value::Str(k)) => Ok(m.get(k).cloned().unwrap_or(Value::Null)),
            (Value::Map(_), _) => Err(CypherError::TypeError(
                "object index must resolve to a string value".into(),
            )),
            _ => Err(CypherError::TypeError(
                "scalar object must be a vertex or edge".into(),
            )),
        }
    }

    fn eval_list_slice(&self, ls: &ListSlice, row: &BindingRow) -> Result<Value, CypherError> {
        let target = self.eval(&ls.expr, row)?;
        let items = match target {
            Value::List(items) => items,
            Value::Null => return Ok(Value::Null),
            _ => {
                return Err(CypherError::TypeError("slice must access a list".into()));
            }
        };
        let len = usize_to_i64(items.len(), "list length")?;
        let resolve = |expr: &Option<Box<CypherExpr>>, default: i64| -> Result<i64, CypherError> {
            match expr {
                Some(e) => match self.eval(e, row)? {
                    Value::Int(n) => Ok(if n < 0 { (len + n).max(0) } else { n.min(len) }),
                    Value::Null => Ok(default),
                    _ => Err(CypherError::TypeError(
                        "slice bound must resolve to an integer value".into(),
                    )),
                },
                None => Ok(default),
            }
        };
        let start = resolve(&ls.start, 0)?;
        let end = resolve(&ls.end, len)?;
        if start >= end {
            return Ok(Value::List(Vec::new()));
        }
        let start = nonnegative_i64_to_usize(start, "slice start")?;
        let end = nonnegative_i64_to_usize(end, "slice end")?;
        Ok(Value::List(items[start..end].to_vec()))
    }

    fn eval_case(&self, c: &CaseExpr, row: &BindingRow) -> Result<Value, CypherError> {
        let operand_value = match &c.operand {
            Some(expr) => Some(self.eval(expr, row)?),
            None => None,
        };
        for (cond, result) in &c.whens {
            let matched = if let Some(operand) = &operand_value {
                let candidate = self.eval(cond, row)?;
                *operand != Value::Null
                    && candidate != Value::Null
                    && agtype::eq(&candidate, operand)
            } else {
                self.eval_predicate(cond, row)?
            };
            if matched {
                return self.eval(result, row);
            }
        }
        if let Some(else_expr) = &c.else_expr {
            return self.eval(else_expr, row);
        }
        Ok(Value::Null)
    }

    #[allow(clippy::too_many_lines)]
    fn eval_function(&self, fc: &FunctionCall, row: &BindingRow) -> Result<Value, CypherError> {
        // Aggregates are handled in the RETURN/WITH path.
        if is_aggregate_name(&fc.name) {
            return Err(CypherError::Unsupported(format!(
                "aggregate {} outside of RETURN/WITH",
                fc.name
            )));
        }
        let name = fc.name.to_lowercase();
        // `exists(n.prop)` needs the unevaluated property expression.
        if name == "exists" {
            let value = match fc.args.first() {
                Some(arg) => self.eval(arg, row)?,
                None => Value::Null,
            };
            return Ok(Value::Bool(value != Value::Null));
        }
        let args: Vec<Value> = fc
            .args
            .iter()
            .map(|a| self.eval(a, row))
            .collect::<Result<_, _>>()?;
        let arg = args.first();
        match name.as_str() {
            "id" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(v) => agtype::entity_id(v).map(Value::Int).ok_or_else(|| {
                    CypherError::TypeError("id() argument must be a vertex, edge or null".into())
                }),
            },
            "label" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(v) => agtype::entity_label(v)
                    .map(|label| Value::Str(label.to_string()))
                    .ok_or_else(|| {
                        CypherError::TypeError(
                            "label() argument must resolve to an edge or vertex".into(),
                        )
                    }),
            },
            "labels" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(v) if agtype::entity_kind(v) == Some(agtype::EntityKind::Vertex) => {
                    Ok(Value::List(vec![Value::Str(
                        agtype::entity_label(v).unwrap_or_default().to_string(),
                    )]))
                }
                Some(_) => Err(CypherError::TypeError(
                    "labels() argument must be a vertex".into(),
                )),
            },
            "type" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(v) if agtype::entity_kind(v) == Some(agtype::EntityKind::Edge) => Ok(
                    Value::Str(agtype::entity_label(v).unwrap_or_default().to_string()),
                ),
                Some(_) => Err(CypherError::TypeError(
                    "type() argument must be an edge or null".into(),
                )),
            },
            "keys" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(v) => {
                    let map = agtype::entity_properties(v).cloned().or_else(|| match v {
                        Value::Map(m) => Some(m.clone()),
                        _ => None,
                    });
                    match map {
                        Some(map) => {
                            let mut keys: Vec<&String> = map.keys().collect();
                            keys.sort_by(|a, b| agtype::jsonb_key_cmp(a, b));
                            Ok(Value::List(
                                keys.into_iter().map(|k| Value::Str(k.clone())).collect(),
                            ))
                        }
                        None => Err(CypherError::TypeError(
                            "keys() argument must be a vertex, edge, map, or null".into(),
                        )),
                    }
                }
            },
            "properties" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(v) => match agtype::entity_properties(v) {
                    Some(props) => Ok(Value::Map(props.clone())),
                    None => match v {
                        Value::Map(m) => Ok(Value::Map(m.clone())),
                        _ => Err(CypherError::TypeError(
                            "properties() argument must be a vertex, an edge or null".into(),
                        )),
                    },
                },
            },
            "startnode" | "endnode" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(v) if agtype::entity_kind(v) == Some(agtype::EntityKind::Edge) => {
                    let id = if name == "startnode" {
                        agtype::edge_start_id(v)
                    } else {
                        agtype::edge_end_id(v)
                    };
                    let Some(id) = id else {
                        return Err(CypherError::Storage(
                            "edge entity is missing a valid endpoint id".into(),
                        ));
                    };
                    let id = nonnegative_i64_to_u64(id, "edge endpoint id")?;
                    match self.store.get_vertex(id) {
                        Some(vertex) => Ok(agtype::vertex_to_value(vertex)?),
                        None => Ok(Value::Null),
                    }
                }
                Some(_) => Err(CypherError::TypeError(format!(
                    "{}() argument must be an edge or null",
                    if name == "startnode" {
                        "startNode"
                    } else {
                        "endNode"
                    }
                ))),
            },
            "length" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(v) if agtype::entity_kind(v) == Some(agtype::EntityKind::Path) => {
                    let elements = validated_path_elements(v)?;
                    Ok(Value::Int(usize_to_i64(
                        (elements.len() - 1) / 2,
                        "path length",
                    )?))
                }
                Some(Value::List(_) | Value::Map(_)) => Err(CypherError::TypeError(
                    "length() argument must resolve to a scalar".into(),
                )),
                Some(_) => Err(CypherError::TypeError(
                    "length() argument must resolve to a path or null".into(),
                )),
            },
            "size" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(Value::List(items)) => Ok(Value::Int(usize_to_i64(items.len(), "list size")?)),
                // AGE's size() counts string BYTES, not characters.
                Some(Value::Str(s)) => Ok(Value::Int(usize_to_i64(s.len(), "string byte size")?)),
                Some(_) => Err(CypherError::TypeError("size() unsupported argument".into())),
            },
            "coalesce" => {
                for v in &args {
                    if *v != Value::Null {
                        return Ok(v.clone());
                    }
                }
                Ok(Value::Null)
            }
            "head" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(Value::List(items)) => Ok(items.first().cloned().unwrap_or(Value::Null)),
                Some(_) => Err(CypherError::TypeError(
                    "head() argument must resolve to a list or null".into(),
                )),
            },
            "last" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(Value::List(items)) => Ok(items.last().cloned().unwrap_or(Value::Null)),
                Some(_) => Err(CypherError::TypeError(
                    "last() argument must resolve to a list or null".into(),
                )),
            },
            "tail" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(Value::List(items)) => {
                    Ok(Value::List(items.iter().skip(1).cloned().collect()))
                }
                Some(_) => Err(CypherError::TypeError(
                    "tail() argument must resolve to a list or null".into(),
                )),
            },
            "reverse" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(Value::Str(s)) => Ok(Value::Str(s.chars().rev().collect())),
                Some(Value::List(items)) => Ok(Value::List(items.iter().rev().cloned().collect())),
                Some(v) => Err(unsupported_argument("reverse", v)),
            },
            "toupper" => string_fn(arg, "toUpper", str::to_uppercase),
            "tolower" => string_fn(arg, "toLower", str::to_lowercase),
            "trim" => string_fn(arg, "trim", |s| s.trim().to_string()),
            "ltrim" => string_fn(arg, "lTrim", |s| s.trim_start().to_string()),
            "rtrim" => string_fn(arg, "rTrim", |s| s.trim_end().to_string()),
            "left" | "right" => {
                let (Some(first), second) = (args.first(), args.get(1)) else {
                    return Err(unsupported_argument(&name, &Value::Null));
                };
                match (first, second) {
                    (Value::Null, _) => Ok(Value::Null),
                    (Value::Str(s), Some(Value::Int(n))) => {
                        if *n < 0 {
                            return Err(CypherError::TypeError(format!(
                                "{name}() negative values are not supported for length"
                            )));
                        }
                        let n = nonnegative_i64_to_usize(*n, &format!("{name} length"))?;
                        let chars: Vec<char> = s.chars().collect();
                        let taken: String = if name == "left" {
                            chars.iter().take(n).collect()
                        } else {
                            let skip = chars.len().saturating_sub(n);
                            chars.iter().skip(skip).collect()
                        };
                        Ok(Value::Str(taken))
                    }
                    (v, _) => Err(unsupported_argument(&name, v)),
                }
            }
            "substring" => {
                let Some(first) = args.first() else {
                    return Err(unsupported_argument("substring", &Value::Null));
                };
                match first {
                    Value::Null => Ok(Value::Null),
                    Value::Str(s) => {
                        let start = match args.get(1) {
                            Some(Value::Int(n)) => *n,
                            Some(Value::Null) | None => return Ok(Value::Null),
                            Some(v) => return Err(unsupported_argument("substring", v)),
                        };
                        let count = match args.get(2) {
                            Some(Value::Int(n)) => Some(*n),
                            None => None,
                            Some(Value::Null) => return Ok(Value::Null),
                            Some(v) => return Err(unsupported_argument("substring", v)),
                        };
                        if start < 0 || count.is_some_and(|c| c < 0) {
                            return Err(CypherError::TypeError(
                                "substring() negative values are not supported for offset or length"
                                    .into(),
                            ));
                        }
                        let chars: Vec<char> = s.chars().collect();
                        let start = nonnegative_i64_to_usize(start, "substring offset")?;
                        let out: String = match count {
                            Some(c) => chars
                                .iter()
                                .skip(start)
                                .take(nonnegative_i64_to_usize(c, "substring length")?)
                                .collect(),
                            None => chars.iter().skip(start).collect(),
                        };
                        Ok(Value::Str(out))
                    }
                    v => Err(unsupported_argument("substring", v)),
                }
            }
            "split" => match (args.first(), args.get(1)) {
                (Some(Value::Null) | None, _) | (_, Some(Value::Null) | None) => Ok(Value::Null),
                (Some(Value::Str(s)), Some(Value::Str(sep))) => Ok(Value::List(
                    s.split(sep.as_str())
                        .map(|part| Value::Str(part.to_string()))
                        .collect(),
                )),
                (Some(v), _) => Err(unsupported_argument("split", v)),
            },
            "replace" => match (args.first(), args.get(1), args.get(2)) {
                (Some(Value::Null) | None, _, _)
                | (_, Some(Value::Null), _)
                | (_, _, Some(Value::Null)) => Ok(Value::Null),
                (Some(Value::Str(s)), Some(Value::Str(from)), Some(Value::Str(to))) => {
                    Ok(Value::Str(s.replace(from.as_str(), to.as_str())))
                }
                (Some(v), _, _) => Err(unsupported_argument("replace", v)),
            },
            "tostring" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(Value::Str(s)) => Ok(Value::Str(s.clone())),
                Some(Value::Int(n)) => Ok(Value::Str(n.to_string())),
                // AGE's toString uses raw float8out (no `.0` suffix).
                Some(Value::Float(f)) => Ok(Value::Str(agtype::format_float_pg(*f))),
                Some(Value::Bool(b)) => Ok(Value::Str(b.to_string())),
                Some(Value::Decimal(d)) => Ok(Value::Str(d.to_sql_string())),
                Some(v) => Err(unsupported_argument("toString", v)),
            },
            "tointeger" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(Value::Int(n)) => Ok(Value::Int(*n)),
                // toInteger truncates toward zero (AGE: toInteger(-4.9) = -4).
                Some(Value::Float(f)) => Ok(Value::Int(trunc_f64_to_i64(*f, "toInteger input")?)),
                Some(Value::Str(s)) => {
                    let input = s.trim();
                    if let Ok(value) = input.parse::<i64>() {
                        Ok(Value::Int(value))
                    } else if let Ok(value) = input.parse::<f64>() {
                        Ok(Value::Int(trunc_f64_to_i64(value, "toInteger input")?))
                    } else {
                        Ok(Value::Null)
                    }
                }
                Some(v) => Err(unsupported_argument("toInteger", v)),
            },
            "tofloat" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(Value::Int(n)) => Ok(Value::Float(exact_i64_to_f64(*n, "toFloat input")?)),
                Some(Value::Float(f)) => Ok(Value::Float(*f)),
                Some(Value::Str(s)) => {
                    Ok(s.trim().parse::<f64>().map_or(Value::Null, Value::Float))
                }
                Some(v) => Err(unsupported_argument("toFloat", v)),
            },
            "toboolean" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(Value::Bool(b)) => Ok(Value::Bool(*b)),
                Some(Value::Int(n)) => Ok(Value::Bool(*n != 0)),
                Some(Value::Str(s)) => Ok(match s.to_lowercase().as_str() {
                    "true" => Value::Bool(true),
                    "false" => Value::Bool(false),
                    _ => Value::Null,
                }),
                Some(v) => Err(unsupported_argument("toBoolean", v)),
            },
            "abs" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(Value::Int(n)) => Ok(Value::Int(n.wrapping_abs())),
                Some(Value::Float(f)) => Ok(Value::Float(f.abs())),
                Some(v) => Err(unsupported_argument("abs", v)),
            },
            "sign" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(Value::Int(n)) => Ok(Value::Int(n.signum())),
                Some(Value::Float(f)) => Ok(Value::Int(if *f > 0.0 {
                    1
                } else if *f < 0.0 {
                    -1
                } else {
                    0
                })),
                Some(v) => Err(unsupported_argument("sign", v)),
            },
            "ceil" => float_fn(arg, "ceil", f64::ceil),
            "floor" => float_fn(arg, "floor", f64::floor),
            "round" => float_fn(arg, "round", f64::round),
            "sqrt" => domain_float_fn(arg, "sqrt", |f| (f >= 0.0).then(|| f.sqrt())),
            "log" => domain_float_fn(arg, "log", |f| (f > 0.0).then(|| f.ln())),
            "log10" => domain_float_fn(arg, "log10", |f| (f > 0.0).then(|| f.log10())),
            "exp" => float_fn(arg, "exp", f64::exp),
            "e" => Ok(Value::Float(std::f64::consts::E)),
            "pi" => Ok(Value::Float(std::f64::consts::PI)),
            "range" => {
                let (start, end) = match (args.first(), args.get(1)) {
                    (Some(Value::Int(a)), Some(Value::Int(b))) => (*a, *b),
                    _ => {
                        return Err(CypherError::TypeError(
                            "range() unsupported argument type".into(),
                        ));
                    }
                };
                let step = match args.get(2) {
                    Some(Value::Int(s)) => *s,
                    None => 1,
                    Some(_) => {
                        return Err(CypherError::TypeError(
                            "range() unsupported argument type".into(),
                        ));
                    }
                };
                if step == 0 {
                    return Err(CypherError::TypeError(
                        "range(): step cannot be zero".into(),
                    ));
                }
                let mut out = Vec::new();
                let mut current = start;
                // range() is end-INCLUSIVE in AGE.
                while (step > 0 && current <= end) || (step < 0 && current >= end) {
                    out.push(Value::Int(current));
                    if current == end {
                        break;
                    }
                    let Some(next) = current.checked_add(step) else {
                        break;
                    };
                    current = next;
                }
                Ok(Value::List(out))
            }
            "timestamp" => {
                let duration = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|error| {
                        CypherError::Storage(format!("system clock precedes Unix epoch: {error}"))
                    })?;
                let ms = i64::try_from(duration.as_millis()).map_err(|_| {
                    CypherError::TypeError("timestamp exceeds agtype integer range".into())
                })?;
                Ok(Value::Int(ms))
            }
            "nodes" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(v) if agtype::entity_kind(v) == Some(agtype::EntityKind::Path) => {
                    let elements = validated_path_elements(v)?;
                    Ok(Value::List(elements.iter().step_by(2).cloned().collect()))
                }
                Some(_) => Err(CypherError::TypeError(
                    "nodes() argument must be a path".into(),
                )),
            },
            "relationships" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(v) if agtype::entity_kind(v) == Some(agtype::EntityKind::Path) => {
                    let elements = validated_path_elements(v)?;
                    Ok(Value::List(
                        elements.iter().skip(1).step_by(2).cloned().collect(),
                    ))
                }
                Some(_) => Err(CypherError::TypeError(
                    "relationships() argument must be a path".into(),
                )),
            },
            other => Err(CypherError::Unsupported(format!("function {other}"))),
        }
    }
}

// ------------------------------------------------------------------
// Helpers
// ------------------------------------------------------------------

fn validated_path_elements(value: &Value) -> Result<&[Value], CypherError> {
    let elements = agtype::path_elements(value)
        .ok_or_else(|| CypherError::Storage("path entity is missing its elements".into()))?;
    if elements.is_empty() || elements.len() % 2 == 0 {
        return Err(CypherError::Storage(format!(
            "path entity has invalid element count {}",
            elements.len()
        )));
    }
    for (index, element) in elements.iter().enumerate() {
        let expected = if index % 2 == 0 {
            agtype::EntityKind::Vertex
        } else {
            agtype::EntityKind::Edge
        };
        if agtype::entity_kind(element) != Some(expected) {
            return Err(CypherError::Storage(format!(
                "path entity element {index} is not a {}",
                if index % 2 == 0 {
                    "vertex"
                } else {
                    "relationship"
                }
            )));
        }
    }
    Ok(elements)
}

/// Variables declared by a set of path patterns (node, relationship,
/// and path variables), used to pad OPTIONAL MATCH misses with nulls.
fn pattern_variables(patterns: &[PathPattern]) -> Vec<String> {
    let mut vars = Vec::new();
    for pattern in patterns {
        if let Some(v) = &pattern.variable {
            vars.push(v.clone());
        }
        for element in &pattern.elements {
            match element {
                PathElement::Node(np) => {
                    if let Some(v) = &np.variable {
                        vars.push(v.clone());
                    }
                }
                PathElement::Rel(rp) => {
                    if let Some(v) = &rp.variable {
                        vars.push(v.clone());
                    }
                }
            }
        }
    }
    vars
}

fn null_or_bool(lhs: &Value, rhs: &Value, result: bool) -> Value {
    if *lhs == Value::Null || *rhs == Value::Null {
        Value::Null
    } else {
        Value::Bool(result)
    }
}

fn sort_keyed<R>(keyed: &mut [(Vec<Value>, R)], order: &[OrderByItem]) {
    keyed.sort_by(|a, b| {
        for (i, (av, bv)) in a.0.iter().zip(b.0.iter()).enumerate() {
            let cmp = agtype::cmp(av, bv);
            let cmp = if order.get(i).is_some_and(|o| !o.ascending) {
                cmp.reverse()
            } else {
                cmp
            };
            if cmp != std::cmp::Ordering::Equal {
                return cmp;
            }
        }
        std::cmp::Ordering::Equal
    });
}

fn return_label(item: &ReturnItem, position: usize) -> String {
    if let Some(alias) = &item.alias {
        return alias.clone();
    }
    match &item.expr {
        CypherExpr::Variable(v) => v.name.clone(),
        CypherExpr::PropertyAccess(p) => format!("{}.{}", p.variable, p.keys.join(".")),
        CypherExpr::FunctionCall(f) => f.name.clone(),
        _ => format!("expr_{position}"),
    }
}

fn is_aggregate(expr: &CypherExpr) -> bool {
    if let CypherExpr::FunctionCall(fc) = expr {
        is_aggregate_name(&fc.name)
    } else {
        false
    }
}

fn is_aggregate_name(name: &str) -> bool {
    matches!(
        name.to_lowercase().as_str(),
        "count" | "sum" | "avg" | "min" | "max" | "collect"
    )
}

fn number_as_f64(v: &Value) -> Result<Option<f64>, CypherError> {
    match v {
        Value::Int(n) => exact_i64_to_f64(*n, "integer operand").map(Some),
        Value::Float(f) => Ok(Some(*f)),
        Value::Decimal(d) => d.to_f64().map(Some).ok_or_else(|| {
            CypherError::TypeError(format!(
                "numeric value {d:?} cannot be represented as a float"
            ))
        }),
        _ => Ok(None),
    }
}

/// min / max over non-null values in agtype order, preserving the
/// original value type. Empty input yields null.
fn aggregate_extreme(values: &[Value], want_min: bool) -> Value {
    let mut best: Option<&Value> = None;
    for v in values {
        let replace = match best {
            None => true,
            Some(current) => {
                let cmp = agtype::cmp(v, current);
                if want_min {
                    cmp == std::cmp::Ordering::Less
                } else {
                    cmp == std::cmp::Ordering::Greater
                }
            }
        };
        if replace {
            best = Some(v);
        }
    }
    best.cloned().unwrap_or(Value::Null)
}

/// sum keeps integer typing while every input is an integer (AGE:
/// `sum([1,2,3])` = 6, `sum([1,2.5])` = 3.5); empty input yields null.
fn aggregate_sum(values: &[Value]) -> Result<Value, CypherError> {
    if values.is_empty() {
        return Ok(Value::Null);
    }
    if values.iter().all(|value| matches!(value, Value::Int(_))) {
        let mut sum = 0_i64;
        for value in values {
            let Value::Int(integer) = value else {
                return Err(CypherError::Storage(
                    "integer aggregate validation became inconsistent".into(),
                ));
            };
            sum = sum.wrapping_add(*integer);
        }
        return Ok(Value::Int(sum));
    }

    let mut sum = 0.0;
    for value in values {
        sum += number_as_f64(value)?
            .ok_or_else(|| CypherError::TypeError("arguments must resolve to a number".into()))?;
    }
    Ok(Value::Float(sum))
}

fn aggregate_avg(values: &[Value]) -> Result<Value, CypherError> {
    if values.is_empty() {
        return Ok(Value::Null);
    }
    let mut total = 0.0;
    for v in values {
        total += number_as_f64(v)?
            .ok_or_else(|| CypherError::TypeError("arguments must resolve to a number".into()))?;
    }
    let count = exact_i64_to_f64(
        usize_to_i64(values.len(), "average count")?,
        "average count",
    )?;
    Ok(Value::Float(total / count))
}

/// Concatenation contribution of a scalar joined to a string with `+`.
/// AGE quirk (verified): booleans contribute an empty string.
fn concat_fragment(v: &Value) -> Option<String> {
    match v {
        Value::Str(s) => Some(s.clone()),
        Value::Int(n) => Some(n.to_string()),
        Value::Float(f) => Some(agtype::format_float_pg(*f)),
        Value::Bool(_) => Some(String::new()),
        _ => None,
    }
}

fn agtype_add(lhs: &Value, rhs: &Value) -> Result<Value, CypherError> {
    if *lhs == Value::Null || *rhs == Value::Null {
        return Ok(Value::Null);
    }
    match (lhs, rhs) {
        (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a.wrapping_add(*b))),
        (Value::List(a), Value::List(b)) => {
            let mut out = a.clone();
            out.extend(b.iter().cloned());
            Ok(Value::List(out))
        }
        // `[1, 2] + 3` appends, `3 + [1, 2]` prepends.
        (Value::List(a), b) => {
            let mut out = a.clone();
            out.push(b.clone());
            Ok(Value::List(out))
        }
        (a, Value::List(b)) => {
            let mut out = vec![a.clone()];
            out.extend(b.iter().cloned());
            Ok(Value::List(out))
        }
        (Value::Map(a), Value::Map(b)) => {
            let mut out = a.clone();
            for (k, v) in b {
                out.insert(k.clone(), v.clone());
            }
            Ok(Value::Map(out))
        }
        (Value::Str(_), _) | (_, Value::Str(_)) => {
            match (concat_fragment(lhs), concat_fragment(rhs)) {
                (Some(a), Some(b)) => Ok(Value::Str(format!("{a}{b}"))),
                _ => Err(CypherError::TypeError(
                    "Invalid input parameter types for agtype_add".into(),
                )),
            }
        }
        _ => match (number_as_f64(lhs)?, number_as_f64(rhs)?) {
            (Some(a), Some(b)) => Ok(Value::Float(a + b)),
            _ => Err(CypherError::TypeError(
                "Invalid input parameter types for agtype_add".into(),
            )),
        },
    }
}

fn numeric_op(
    lhs: &Value,
    rhs: &Value,
    age_name: &str,
    f_int: impl Fn(i64, i64) -> i64,
    f_float: impl Fn(f64, f64) -> f64,
) -> Result<Value, CypherError> {
    if *lhs == Value::Null || *rhs == Value::Null {
        return Ok(Value::Null);
    }
    if let (Value::Int(a), Value::Int(b)) = (lhs, rhs) {
        return Ok(Value::Int(f_int(*a, *b)));
    }
    match (number_as_f64(lhs)?, number_as_f64(rhs)?) {
        (Some(a), Some(b)) => Ok(Value::Float(f_float(a, b))),
        _ => Err(CypherError::TypeError(format!(
            "Invalid input parameter types for {age_name}"
        ))),
    }
}

fn agtype_div(lhs: &Value, rhs: &Value) -> Result<Value, CypherError> {
    if *lhs == Value::Null || *rhs == Value::Null {
        return Ok(Value::Null);
    }
    if let (Value::Int(a), Value::Int(b)) = (lhs, rhs) {
        if *b == 0 {
            return Err(CypherError::TypeError("division by zero".into()));
        }
        return Ok(Value::Int(a.wrapping_div(*b)));
    }
    match (number_as_f64(lhs)?, number_as_f64(rhs)?) {
        (Some(a), Some(b)) => {
            if b == 0.0 {
                return Err(CypherError::TypeError("division by zero".into()));
            }
            Ok(Value::Float(a / b))
        }
        _ => Err(CypherError::TypeError(
            "Invalid input parameter types for agtype_div".into(),
        )),
    }
}

fn agtype_mod(lhs: &Value, rhs: &Value) -> Result<Value, CypherError> {
    if *lhs == Value::Null || *rhs == Value::Null {
        return Ok(Value::Null);
    }
    if let (Value::Int(a), Value::Int(b)) = (lhs, rhs) {
        // AGE quirk (verified on 1.6.0): integer modulo by zero
        // returns the dividend instead of raising.
        if *b == 0 {
            return Ok(Value::Int(*a));
        }
        return Ok(Value::Int(a.wrapping_rem(*b)));
    }
    match (number_as_f64(lhs)?, number_as_f64(rhs)?) {
        // fmod semantics: sign follows the dividend; x % 0.0 = NaN.
        (Some(a), Some(b)) => Ok(Value::Float(a % b)),
        _ => Err(CypherError::TypeError(
            "Invalid input parameter types for agtype_mod".into(),
        )),
    }
}

fn agtype_pow(lhs: &Value, rhs: &Value) -> Result<Value, CypherError> {
    if *lhs == Value::Null || *rhs == Value::Null {
        return Ok(Value::Null);
    }
    match (number_as_f64(lhs)?, number_as_f64(rhs)?) {
        // `^` ALWAYS yields a float in AGE (2^2 = 4.0).
        (Some(a), Some(b)) => Ok(Value::Float(a.powf(b))),
        _ => Err(CypherError::TypeError(
            "Invalid input parameter types for agtype_pow".into(),
        )),
    }
}

/// STARTS WITH / ENDS WITH / CONTAINS: null propagates, non-string
/// operands compare false (verified: `'abc' STARTS WITH 1` = false).
fn str_predicate(lhs: &Value, rhs: &Value, f: impl Fn(&str, &str) -> bool) -> Value {
    match (lhs, rhs) {
        (Value::Null, _) | (_, Value::Null) => Value::Null,
        (Value::Str(a), Value::Str(b)) => Value::Bool(f(a, b)),
        _ => Value::Bool(false),
    }
}

/// `=~` is an UNANCHORED regular-expression search in AGE
/// (`PostgreSQL` `~` semantics): `'abc' =~ 'b'` is true. Non-string
/// operands (including null) yield null.
fn regex_match(lhs: &Value, rhs: &Value) -> Result<Value, CypherError> {
    match (lhs, rhs) {
        (Value::Str(a), Value::Str(pattern)) => {
            let re = regex::Regex::new(pattern)
                .map_err(|e| CypherError::TypeError(format!("invalid regular expression: {e}")))?;
            Ok(Value::Bool(re.is_match(a)))
        }
        _ => Ok(Value::Null),
    }
}

fn unsupported_argument(function: &str, value: &Value) -> CypherError {
    CypherError::TypeError(format!(
        "{function}() unsupported argument agtype {}",
        agtype::agtype_type_ordinal(value)
    ))
}

fn string_fn(
    arg: Option<&Value>,
    name: &str,
    f: impl Fn(&str) -> String,
) -> Result<Value, CypherError> {
    match arg {
        Some(Value::Null) | None => Ok(Value::Null),
        Some(Value::Str(s)) => Ok(Value::Str(f(s))),
        Some(v) => Err(unsupported_argument(name, v)),
    }
}

/// Numeric function that always yields a float (AGE: `ceil(2)` = 2.0).
fn float_fn(arg: Option<&Value>, name: &str, f: impl Fn(f64) -> f64) -> Result<Value, CypherError> {
    match arg {
        Some(Value::Null) | None => Ok(Value::Null),
        Some(v) => match number_as_f64(v)? {
            Some(x) => Ok(Value::Float(f(x))),
            None => Err(unsupported_argument(name, v)),
        },
    }
}

/// Numeric function with a restricted domain; out-of-domain inputs
/// return null (AGE: `sqrt(-1)` = null, `log(0)` = null).
fn domain_float_fn(
    arg: Option<&Value>,
    name: &str,
    f: impl Fn(f64) -> Option<f64>,
) -> Result<Value, CypherError> {
    match arg {
        Some(Value::Null) | None => Ok(Value::Null),
        Some(v) => match number_as_f64(v)? {
            Some(x) => Ok(f(x).map_or(Value::Null, Value::Float)),
            None => Err(unsupported_argument(name, v)),
        },
    }
}
