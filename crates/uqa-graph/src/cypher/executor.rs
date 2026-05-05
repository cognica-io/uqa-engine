//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read-only Cypher executor: walks a `CypherQuery` AST and lowers it
//! onto graph operators against a [`GraphStore`].
//!
//! Supported clauses: `MATCH` (node, 1-hop rel, variable-length rel),
//! `OPTIONAL MATCH`, `WHERE`, `RETURN` (with `DISTINCT`, `ORDER BY`,
//! `SKIP`, `LIMIT`), and `WITH` as a RETURN-as-pipeline step. Function
//! calls cover the basic aggregates (`count`, `sum`, `avg`, `min`,
//! `max`, `collect`) and a small set of scalar helpers (`length`,
//! `id`, `labels`, `type`, `keys`, `toUpper`, `toLower`).
//!
//! Mutation clauses (`CREATE`, `SET`, `MERGE`, `DELETE`, `UNWIND`)
//! land in a follow-up slice; the executor returns
//! `CypherError::Unsupported` for those.

use std::collections::{BTreeMap, BTreeSet};

use uqa_core::{Edge, EdgeId, Value, Vertex, VertexId};

use crate::cypher::ast::{
    BinaryOp, CaseExpr, CypherClause, CypherExpr, CypherQuery, FunctionCall, InList, IsNotNull,
    IsNull, ListIndex, ListLiteral, Literal, MatchClause, NodePattern, OrderByItem, Parameter,
    PathElement, PathPattern, PropertyAccess, RelDirection, RelPattern, ReturnItem, UnaryOp,
    Variable,
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
            Binding::Value(Value::Map(m)) => m.get(key).cloned().unwrap_or(Value::Null),
            _ => Value::Null,
        }
    }

    fn to_value(&self) -> Value {
        match self {
            Binding::Vertex(v) => {
                let mut m = v.properties.clone();
                m.insert("_id".into(), Value::Int(v.vertex_id as i64));
                m.insert("_label".into(), Value::Str(v.label.clone()));
                m.insert("_kind".into(), Value::Str("vertex".into()));
                Value::Map(m)
            }
            Binding::Edge(e) => {
                let mut m = e.properties.clone();
                m.insert("_id".into(), Value::Int(e.edge_id as i64));
                m.insert("_label".into(), Value::Str(e.label.clone()));
                m.insert("_source".into(), Value::Int(e.source_id as i64));
                m.insert("_target".into(), Value::Int(e.target_id as i64));
                m.insert("_kind".into(), Value::Str("edge".into()));
                Value::Map(m)
            }
            Binding::Value(v) => v.clone(),
            Binding::EdgeList(edges) => Value::List(
                edges
                    .iter()
                    .map(|e| Binding::Edge(e.clone()).to_value())
                    .collect(),
            ),
        }
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
    #[error("type error: {0}")]
    TypeError(String),
}

/// Read-only execution context.
pub struct CypherExecutor<'a, G: GraphStore> {
    pub store: &'a G,
    pub graph: &'a str,
    pub params: BTreeMap<String, Value>,
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
                    if let Some(filter) = &w.r#where {
                        bindings = projected
                            .into_iter()
                            .filter(|row| self.where_passes(filter, row).unwrap_or(false))
                            .map(|row| Self::row_to_bindings(&cols, &row))
                            .collect();
                    } else {
                        bindings = projected
                            .into_iter()
                            .map(|row| Self::row_to_bindings(&cols, &row))
                            .collect();
                    }
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
            if current.is_empty() && clause.optional {
                next.push(row.clone());
            } else {
                next.extend(current);
            }
        }
        if let Some(filter) = &clause.r#where {
            next.retain(|row| self.eval_predicate(filter, row).unwrap_or(false));
        }
        Ok(next)
    }

    pub(crate) fn match_path_pattern(
        &self,
        pattern: &PathPattern,
        seed: &BindingRow,
    ) -> Result<Vec<BindingRow>, CypherError> {
        let mut frontier: Vec<BindingRow> = vec![seed.clone()];
        let mut last_var: Option<String> = None;
        let mut idx = 0;
        while idx < pattern.elements.len() {
            match &pattern.elements[idx] {
                PathElement::Node(np) => {
                    frontier = self.bind_node(np, &frontier, last_var.as_deref())?;
                    last_var.clone_from(&np.variable);
                    idx += 1;
                }
                PathElement::Rel(rp) => {
                    let Some(PathElement::Node(next_node)) = pattern.elements.get(idx + 1) else {
                        return Err(CypherError::Unsupported("path must end on a node".into()));
                    };
                    let prev = last_var.as_deref().ok_or_else(|| {
                        CypherError::Unsupported("relationship without prior node".into())
                    })?;
                    frontier = self.traverse_rel(rp, next_node, &frontier, prev)?;
                    last_var.clone_from(&next_node.variable);
                    idx += 2;
                }
            }
        }
        Ok(frontier)
    }

    fn bind_node(
        &self,
        np: &NodePattern,
        rows: &[BindingRow],
        prior_var: Option<&str>,
    ) -> Result<Vec<BindingRow>, CypherError> {
        let mut out = Vec::new();
        // Candidate vertex set: by label if specified, else everything in the graph.
        let candidate_ids: Vec<VertexId> = if let Some(label) = np.labels.first() {
            self.store
                .vertices_by_label(label, self.graph)
                .into_iter()
                .map(|v| v.vertex_id)
                .collect()
        } else {
            self.store
                .vertex_ids_in_graph(self.graph)
                .into_iter()
                .collect()
        };

        for row in rows {
            for vid in &candidate_ids {
                let Some(vertex) = self.store.get_vertex(*vid).cloned() else {
                    continue;
                };
                if !self.node_matches(np, &vertex, row)? {
                    continue;
                }
                let _ = prior_var; // kept for future cross-checks
                let mut new_row = row.clone();
                if let Some(var) = &np.variable {
                    if let Some(prior) = new_row.get(var) {
                        // Variable already bound — must refer to the same vertex.
                        if let Binding::Vertex(prev) = prior {
                            if prev.vertex_id != vertex.vertex_id {
                                continue;
                            }
                        } else {
                            continue;
                        }
                    } else {
                        new_row.insert(var.clone(), Binding::Vertex(vertex));
                    }
                }
                out.push(new_row);
            }
        }
        Ok(out)
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
                if got != want {
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
                if got != want {
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
        rows: &[BindingRow],
        prev_var: &str,
    ) -> Result<Vec<BindingRow>, CypherError> {
        let direction = match rp.direction {
            RelDirection::Right => Direction::Out,
            RelDirection::Left => Direction::In,
            RelDirection::Both => Direction::Both,
        };
        let is_var_length = rp.min_hops.is_some() || rp.max_hops.is_some();
        let mut out = Vec::new();
        for row in rows {
            let Some(Binding::Vertex(start)) = row.get(prev_var).cloned() else {
                continue;
            };
            if is_var_length {
                let min_hops = rp.min_hops.unwrap_or(1);
                let max_hops = rp.max_hops.unwrap_or(min_hops.max(1));
                let mut buffer: Vec<(VertexId, Vec<Edge>)> = vec![(start.vertex_id, Vec::new())];
                let mut all_paths: Vec<(VertexId, Vec<Edge>)> = Vec::new();
                if min_hops == 0 {
                    all_paths.push((start.vertex_id, Vec::new()));
                }
                for hop in 1..=max_hops {
                    let mut next_buffer = Vec::new();
                    for (vertex_id, edges_so_far) in &buffer {
                        for edge in self.outgoing_edges(*vertex_id, direction) {
                            if !self.rel_matches(rp, &edge, row)? {
                                continue;
                            }
                            let neighbor = if edge.source_id == *vertex_id {
                                edge.target_id
                            } else {
                                edge.source_id
                            };
                            let mut new_edges = edges_so_far.clone();
                            new_edges.push(edge);
                            if hop >= min_hops {
                                all_paths.push((neighbor, new_edges.clone()));
                            }
                            next_buffer.push((neighbor, new_edges));
                        }
                    }
                    buffer = next_buffer;
                    if buffer.is_empty() {
                        break;
                    }
                }
                for (end_id, edges) in all_paths {
                    let Some(end_vertex) = self.store.get_vertex(end_id).cloned() else {
                        continue;
                    };
                    if !self.node_matches(next, &end_vertex, row)? {
                        continue;
                    }
                    let mut new_row = row.clone();
                    if let Some(var) = &rp.variable {
                        new_row.insert(var.clone(), Binding::EdgeList(edges));
                    }
                    if let Some(var) = &next.variable {
                        new_row.insert(var.clone(), Binding::Vertex(end_vertex));
                    }
                    out.push(new_row);
                }
            } else {
                for edge in self.outgoing_edges(start.vertex_id, direction) {
                    if !self.rel_matches(rp, &edge, row)? {
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
                    if !self.node_matches(next, &end_vertex, row)? {
                        continue;
                    }
                    let mut new_row = row.clone();
                    if let Some(var) = &rp.variable {
                        new_row.insert(var.clone(), Binding::Edge(edge));
                    }
                    if let Some(var) = &next.variable {
                        new_row.insert(var.clone(), Binding::Vertex(end_vertex));
                    }
                    out.push(new_row);
                }
            }
        }
        Ok(out)
    }

    fn outgoing_edges(&self, vertex_id: VertexId, direction: Direction) -> Vec<Edge> {
        let mut ids: BTreeSet<EdgeId> = BTreeSet::new();
        match direction {
            Direction::Out => {
                ids.extend(self.store.out_edge_ids(vertex_id, self.graph));
            }
            Direction::In => {
                ids.extend(self.store.in_edge_ids(vertex_id, self.graph));
            }
            Direction::Both => {
                ids.extend(self.store.out_edge_ids(vertex_id, self.graph));
                ids.extend(self.store.in_edge_ids(vertex_id, self.graph));
            }
        }
        ids.into_iter()
            .filter_map(|eid| self.store.get_edge(eid).cloned())
            .collect()
    }

    fn eval_predicate(&self, expr: &CypherExpr, row: &BindingRow) -> Result<bool, CypherError> {
        Ok(value_truthy(&self.eval(expr, row)?))
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
        Ok(value_truthy(&self.eval(expr, &bindings)?))
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
        let columns: Vec<String> = items
            .iter()
            .enumerate()
            .map(|(i, item)| return_label(item, i))
            .collect();

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
                let n = self.eval_int(skip_expr, &BTreeMap::new())? as usize;
                if n >= binding_rows.len() {
                    binding_rows.clear();
                } else {
                    binding_rows.drain(0..n);
                }
            }
            if let Some(limit_expr) = limit {
                let n = self.eval_int(limit_expr, &BTreeMap::new())? as usize;
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
                let n = self.eval_int(skip_expr, &BTreeMap::new())? as usize;
                if n >= rows.len() {
                    rows.clear();
                } else {
                    rows.drain(0..n);
                }
            }
            if let Some(limit_expr) = limit {
                let n = self.eval_int(limit_expr, &BTreeMap::new())? as usize;
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
        match name.as_str() {
            "count" => {
                if fc.args.iter().any(|a| {
                    matches!(a,
                    CypherExpr::Variable(v) if v.name == "*")
                }) {
                    return Ok(Value::Int(members.len() as i64));
                }
                if fc.args.is_empty() {
                    return Ok(Value::Int(members.len() as i64));
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
                            count += 1;
                        }
                    } else {
                        count += 1;
                    }
                }
                Ok(Value::Int(count))
            }
            "sum" | "avg" | "min" | "max" => {
                let mut nums: Vec<f64> = Vec::new();
                for row in members {
                    let v = self.eval(&fc.args[0], row)?;
                    if let Some(n) = value_as_f64(&v) {
                        nums.push(n);
                    }
                }
                if nums.is_empty() {
                    return Ok(Value::Null);
                }
                let result = match name.as_str() {
                    "sum" => nums.iter().sum::<f64>(),
                    "avg" => nums.iter().sum::<f64>() / nums.len() as f64,
                    "min" => nums.iter().copied().fold(f64::INFINITY, f64::min),
                    "max" => nums.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                    _ => unreachable!(),
                };
                Ok(Value::Float(result))
            }
            "collect" => {
                let mut list = Vec::new();
                for row in members {
                    let v = self.eval(&fc.args[0], row)?;
                    if v != Value::Null {
                        list.push(v);
                    }
                }
                Ok(Value::List(list))
            }
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
            Value::Float(f) => Ok(f as i64),
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
                Some(b) => Ok(b.to_value()),
                None => Err(CypherError::UndefinedVariable(name.clone())),
            },
            CypherExpr::PropertyAccess(PropertyAccess { variable, keys }) => {
                let mut value = match row.get(variable) {
                    Some(b) => b.property(&keys[0]),
                    None => return Err(CypherError::UndefinedVariable(variable.clone())),
                };
                for key in &keys[1..] {
                    value = match value {
                        Value::Map(m) => m.get(key).cloned().unwrap_or(Value::Null),
                        _ => Value::Null,
                    };
                }
                Ok(value)
            }
            CypherExpr::BinaryOp(b) => self.eval_binary(b, row),
            CypherExpr::UnaryOp(u) => self.eval_unary(u, row),
            CypherExpr::ListIndex(li) => self.eval_list_index(li, row),
            CypherExpr::ListLiteral(ll) => self.eval_list_literal(ll, row),
            CypherExpr::InList(il) => self.eval_in_list(il, row),
            CypherExpr::IsNull(IsNull { expr }) => {
                Ok(Value::Bool(matches!(self.eval(expr, row)?, Value::Null)))
            }
            CypherExpr::IsNotNull(IsNotNull { expr }) => {
                Ok(Value::Bool(!matches!(self.eval(expr, row)?, Value::Null)))
            }
            CypherExpr::CaseExpr(c) => self.eval_case(c, row),
            CypherExpr::FunctionCall(fc) => self.eval_function(fc, row),
            CypherExpr::MapLiteral(_) => Err(CypherError::Unsupported("map literal".into())),
        }
    }

    fn eval_binary(&self, b: &BinaryOp, row: &BindingRow) -> Result<Value, CypherError> {
        let lhs = self.eval(&b.left, row)?;
        let rhs = self.eval(&b.right, row)?;
        match b.op.as_str() {
            "AND" => Ok(Value::Bool(value_truthy(&lhs) && value_truthy(&rhs))),
            "OR" => Ok(Value::Bool(value_truthy(&lhs) || value_truthy(&rhs))),
            "XOR" => Ok(Value::Bool(value_truthy(&lhs) ^ value_truthy(&rhs))),
            "=" => Ok(Value::Bool(lhs == rhs)),
            "<>" => Ok(Value::Bool(lhs != rhs)),
            "<" => Ok(Value::Bool(lhs < rhs)),
            ">" => Ok(Value::Bool(lhs > rhs)),
            "<=" => Ok(Value::Bool(lhs <= rhs)),
            ">=" => Ok(Value::Bool(lhs >= rhs)),
            "+" => arith(&lhs, &rhs, |a, b| a + b, |a, b| a + b, true),
            "-" => arith(&lhs, &rhs, |a, b| a - b, |a, b| a - b, false),
            "*" => arith(&lhs, &rhs, |a, b| a * b, |a, b| a * b, false),
            "/" => arith(&lhs, &rhs, |a, b| a / b, |a, b| a / b, false),
            "%" => arith(&lhs, &rhs, |a, b| a % b, |a, b| a % b, false),
            "STARTS WITH" => str_pred(&lhs, &rhs, |a, b| a.starts_with(b)),
            "ENDS WITH" => str_pred(&lhs, &rhs, |a, b| a.ends_with(b)),
            "CONTAINS" => str_pred(&lhs, &rhs, |a, b| a.contains(b)),
            other => Err(CypherError::Unsupported(format!("binary op {other}"))),
        }
    }

    fn eval_unary(&self, u: &UnaryOp, row: &BindingRow) -> Result<Value, CypherError> {
        let operand = self.eval(&u.operand, row)?;
        match u.op.as_str() {
            "NOT" => Ok(Value::Bool(!value_truthy(&operand))),
            "-" => match operand {
                Value::Int(n) => Ok(Value::Int(-n)),
                Value::Float(f) => Ok(Value::Float(-f)),
                _ => Err(CypherError::TypeError("unary - on non-numeric".into())),
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

    fn eval_in_list(&self, il: &InList, row: &BindingRow) -> Result<Value, CypherError> {
        let needle = self.eval(&il.expr, row)?;
        let haystack = self.eval(&il.list_expr, row)?;
        Ok(match haystack {
            Value::List(items) => Value::Bool(items.contains(&needle)),
            _ => Value::Bool(false),
        })
    }

    fn eval_list_index(&self, li: &ListIndex, row: &BindingRow) -> Result<Value, CypherError> {
        let target = self.eval(&li.expr, row)?;
        let index = self.eval(&li.index, row)?;
        match (target, index) {
            (Value::List(items), Value::Int(n)) => {
                let idx = if n < 0 {
                    (items.len() as i64 + n) as usize
                } else {
                    n as usize
                };
                Ok(items.get(idx).cloned().unwrap_or(Value::Null))
            }
            (Value::Map(m), Value::Str(k)) => Ok(m.get(&k).cloned().unwrap_or(Value::Null)),
            _ => Ok(Value::Null),
        }
    }

    fn eval_case(&self, c: &CaseExpr, row: &BindingRow) -> Result<Value, CypherError> {
        let operand_value = match &c.operand {
            Some(expr) => Some(self.eval(expr, row)?),
            None => None,
        };
        for (cond, result) in &c.whens {
            let matched = if let Some(operand) = &operand_value {
                self.eval(cond, row)? == *operand
            } else {
                value_truthy(&self.eval(cond, row)?)
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

    fn eval_function(&self, fc: &FunctionCall, row: &BindingRow) -> Result<Value, CypherError> {
        // Aggregates are handled in the RETURN/WITH path.
        if is_aggregate_name(&fc.name) {
            return Err(CypherError::Unsupported(format!(
                "aggregate {} outside of RETURN/WITH",
                fc.name
            )));
        }
        let name = fc.name.to_lowercase();
        let args: Vec<Value> = fc
            .args
            .iter()
            .map(|a| self.eval(a, row))
            .collect::<Result<_, _>>()?;
        match name.as_str() {
            "length" | "size" => Ok(match args.first() {
                Some(Value::List(l)) => Value::Int(l.len() as i64),
                Some(Value::Str(s)) => Value::Int(s.chars().count() as i64),
                _ => Value::Null,
            }),
            "id" => match fc.args.first() {
                Some(CypherExpr::Variable(v)) => match row.get(&v.name) {
                    Some(Binding::Vertex(vertex)) => Ok(Value::Int(vertex.vertex_id as i64)),
                    Some(Binding::Edge(edge)) => Ok(Value::Int(edge.edge_id as i64)),
                    _ => Ok(Value::Null),
                },
                _ => Ok(Value::Null),
            },
            "labels" => match fc.args.first() {
                Some(CypherExpr::Variable(v)) => match row.get(&v.name) {
                    Some(Binding::Vertex(vertex)) => {
                        Ok(Value::List(vec![Value::Str(vertex.label.clone())]))
                    }
                    _ => Ok(Value::List(Vec::new())),
                },
                _ => Ok(Value::List(Vec::new())),
            },
            "type" => match fc.args.first() {
                Some(CypherExpr::Variable(v)) => match row.get(&v.name) {
                    Some(Binding::Edge(edge)) => Ok(Value::Str(edge.label.clone())),
                    _ => Ok(Value::Null),
                },
                _ => Ok(Value::Null),
            },
            "keys" => match fc.args.first() {
                Some(CypherExpr::Variable(v)) => match row.get(&v.name) {
                    Some(Binding::Vertex(vertex)) => Ok(Value::List(
                        vertex.properties.keys().cloned().map(Value::Str).collect(),
                    )),
                    Some(Binding::Edge(edge)) => Ok(Value::List(
                        edge.properties.keys().cloned().map(Value::Str).collect(),
                    )),
                    _ => Ok(Value::List(Vec::new())),
                },
                _ => Ok(Value::List(Vec::new())),
            },
            "toupper" => Ok(string_op(args.first(), str::to_uppercase)),
            "tolower" => Ok(string_op(args.first(), str::to_lowercase)),
            "abs" => Ok(match args.first() {
                Some(Value::Int(n)) => Value::Int(n.abs()),
                Some(Value::Float(f)) => Value::Float(f.abs()),
                _ => Value::Null,
            }),
            other => Err(CypherError::Unsupported(format!("function {other}"))),
        }
    }
}

// ------------------------------------------------------------------
// Helpers
// ------------------------------------------------------------------

fn sort_keyed<R>(keyed: &mut [(Vec<Value>, R)], order: &[OrderByItem]) {
    keyed.sort_by(|a, b| {
        for (i, (av, bv)) in a.0.iter().zip(b.0.iter()).enumerate() {
            let cmp = av.cmp(bv);
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

fn value_truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Null => false,
        Value::Int(n) => *n != 0,
        Value::Float(f) => *f != 0.0,
        Value::Str(s) => !s.is_empty(),
        Value::List(l) => !l.is_empty(),
        Value::Map(m) => !m.is_empty(),
        Value::Bytes(b) => !b.is_empty(),
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

fn arith(
    lhs: &Value,
    rhs: &Value,
    f_int: impl Fn(i64, i64) -> i64,
    f_float: impl Fn(f64, f64) -> f64,
    allow_string_concat: bool,
) -> Result<Value, CypherError> {
    if let (Value::Int(a), Value::Int(b)) = (lhs, rhs) {
        return Ok(Value::Int(f_int(*a, *b)));
    }
    if let (Some(a), Some(b)) = (value_as_f64(lhs), value_as_f64(rhs)) {
        return Ok(Value::Float(f_float(a, b)));
    }
    if allow_string_concat {
        if let (Value::Str(a), Value::Str(b)) = (lhs, rhs) {
            return Ok(Value::Str(format!("{a}{b}")));
        }
    }
    Err(CypherError::TypeError(format!(
        "arithmetic between {lhs:?} and {rhs:?}"
    )))
}

fn str_pred(
    lhs: &Value,
    rhs: &Value,
    f: impl Fn(&str, &str) -> bool,
) -> Result<Value, CypherError> {
    match (lhs, rhs) {
        (Value::Str(a), Value::Str(b)) => Ok(Value::Bool(f(a, b))),
        _ => Err(CypherError::TypeError(
            "string predicate on non-string".into(),
        )),
    }
}

fn string_op(value: Option<&Value>, op: fn(&str) -> String) -> Value {
    match value {
        Some(Value::Str(s)) => Value::Str(op(s)),
        _ => Value::Null,
    }
}
