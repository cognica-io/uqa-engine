//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mutating Cypher executor: extends the read-only [`CypherExecutor`]
//! pipeline with `CREATE`, `SET`, `MERGE`, `DELETE` / `DETACH DELETE`,
//! and `UNWIND` clauses. Reads delegate to a transient
//! [`CypherExecutor`] view that borrows the store immutably.

use std::collections::BTreeMap;

use uqa_core::{Edge, EdgeId, Value, Vertex, VertexId};

use crate::cypher::ast::{
    CreateClause, CypherClause, CypherExpr, CypherQuery, DeleteClause, MergeClause, NodePattern,
    PathElement, PathPattern, PropertyAccess, RelDirection, RelPattern, SetClause, SetItem,
    SetOperator, UnwindClause, Variable,
};
use crate::cypher::executor::{Binding, BindingRow, CypherError, CypherExecutor, ResultRow};
use crate::store::GraphStore;

/// Mutating Cypher executor. Holds a unique borrow of the graph store
/// and a copy of the parameter map; reads are routed through a
/// transient read-only view.
pub struct CypherWriter<'a, G: GraphStore> {
    pub store: &'a mut G,
    pub graph: String,
    pub params: BTreeMap<String, Value>,
}

impl<'a, G: GraphStore> CypherWriter<'a, G> {
    pub fn new(store: &'a mut G, graph: impl Into<String>) -> Self {
        Self {
            store,
            graph: graph.into(),
            params: BTreeMap::new(),
        }
    }

    pub fn with_params(mut self, params: BTreeMap<String, Value>) -> Self {
        self.params = params;
        self
    }

    fn reader(&self) -> CypherExecutor<'_, G> {
        let mut exec = CypherExecutor::new(&*self.store, &self.graph);
        exec.params = self.params.clone();
        exec
    }

    pub fn execute(
        &mut self,
        query: &CypherQuery,
    ) -> Result<(Vec<String>, Vec<ResultRow>), CypherError> {
        let mut bindings: Vec<BindingRow> = vec![BTreeMap::new()];
        let mut columns: Vec<String> = Vec::new();
        let mut rows: Vec<ResultRow> = Vec::new();
        for clause in &query.clauses {
            match clause {
                CypherClause::Match(m) => {
                    bindings = self.reader().exec_match(m, &bindings)?;
                }
                CypherClause::Create(c) => {
                    bindings = self.exec_create(c, bindings)?;
                }
                CypherClause::Merge(m) => {
                    bindings = self.exec_merge(m, bindings)?;
                }
                CypherClause::Set(s) => {
                    bindings = self.exec_set(s, bindings)?;
                }
                CypherClause::Delete(d) => {
                    bindings = self.exec_delete(d, bindings)?;
                }
                CypherClause::Unwind(u) => {
                    bindings = self.exec_unwind(u, bindings)?;
                }
                CypherClause::With(w) => {
                    let (cols, projected) = self.reader().exec_return_like(
                        &w.items,
                        w.distinct,
                        w.order_by.as_deref(),
                        w.skip.as_ref(),
                        w.limit.as_ref(),
                        &bindings,
                    )?;
                    let reader = self.reader();
                    let mut next = Vec::with_capacity(projected.len());
                    for row in projected {
                        if let Some(filter) = &w.r#where {
                            if !reader.where_passes(filter, &row)? {
                                continue;
                            }
                        }
                        next.push(CypherExecutor::<G>::row_to_bindings(&cols, &row));
                    }
                    drop(reader);
                    bindings = next;
                }
                CypherClause::Return(r) => {
                    let (cols, ret_rows) = self.reader().exec_return_like(
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
            }
        }
        Ok((columns, rows))
    }

    // -----------------------------------------------------------------
    // CREATE
    // -----------------------------------------------------------------

    fn exec_create(
        &mut self,
        clause: &CreateClause,
        bindings: Vec<BindingRow>,
    ) -> Result<Vec<BindingRow>, CypherError> {
        let mut next = Vec::with_capacity(bindings.len());
        for mut row in bindings {
            for pattern in &clause.patterns {
                self.create_path(pattern, &mut row)?;
            }
            next.push(row);
        }
        Ok(next)
    }

    fn create_path(
        &mut self,
        pattern: &PathPattern,
        row: &mut BindingRow,
    ) -> Result<(), CypherError> {
        let elements = &pattern.elements;
        // The vertex id the path currently stands on. Anonymous nodes
        // participate positionally without a variable binding.
        let mut position: Option<VertexId> = None;
        let mut idx = 0;
        while idx < elements.len() {
            match &elements[idx] {
                PathElement::Node(np) => {
                    position = Some(self.resolve_or_create_vertex(np, row)?);
                    idx += 1;
                }
                PathElement::Rel(rp) => {
                    let Some(PathElement::Node(next_np)) = elements.get(idx + 1) else {
                        return Err(CypherError::Unsupported("path must end on a node".into()));
                    };
                    let src_id = position.ok_or_else(|| {
                        CypherError::Unsupported("relationship without prior node".into())
                    })?;
                    let tgt_id = self.resolve_or_create_vertex(next_np, row)?;
                    let edge = self.create_edge(rp, row, src_id, tgt_id)?;
                    if let Some(var) = &rp.variable {
                        row.insert(var.clone(), Binding::Edge(edge));
                    }
                    position = Some(tgt_id);
                    idx += 2;
                }
            }
        }
        Ok(())
    }

    /// Vertex id for a CREATE node pattern: reuse the bound vertex when
    /// the variable already resolves to one, otherwise create a fresh
    /// vertex (binding it when a variable is present).
    fn resolve_or_create_vertex(
        &mut self,
        np: &NodePattern,
        row: &mut BindingRow,
    ) -> Result<VertexId, CypherError> {
        if let Some(var) = &np.variable {
            match row.get(var) {
                Some(Binding::Vertex(v)) => return Ok(v.vertex_id),
                Some(_) => {
                    return Err(CypherError::TypeError(format!("{var:?} is not a vertex")));
                }
                None => {}
            }
        }
        let vertex = self.create_vertex(np, row)?;
        let id = vertex.vertex_id;
        if let Some(var) = &np.variable {
            row.insert(var.clone(), Binding::Vertex(vertex));
        }
        Ok(id)
    }

    fn create_vertex(
        &mut self,
        pat: &NodePattern,
        row: &BindingRow,
    ) -> Result<Vertex, CypherError> {
        let label = pat.labels.first().cloned().unwrap_or_default();
        let mut props: BTreeMap<String, Value> = BTreeMap::new();
        if let Some(map) = &pat.properties {
            let reader = self.reader();
            for (k, expr) in map {
                props.insert(k.clone(), reader.eval(expr, row)?);
            }
        }
        // AGE graphid allocation: (label_id << 48) | per-label sequence.
        let vid = self.store.allocate_vertex_id(&label, &self.graph);
        let vertex = Vertex {
            vertex_id: vid,
            label,
            properties: props,
        };
        self.store.add_vertex(vertex.clone(), &self.graph);
        Ok(vertex)
    }

    fn create_edge(
        &mut self,
        pat: &RelPattern,
        row: &BindingRow,
        mut src_id: VertexId,
        mut tgt_id: VertexId,
    ) -> Result<Edge, CypherError> {
        if pat.direction == RelDirection::Left {
            std::mem::swap(&mut src_id, &mut tgt_id);
        }
        let label = pat.types.first().cloned().unwrap_or_default();
        let mut props: BTreeMap<String, Value> = BTreeMap::new();
        if let Some(map) = &pat.properties {
            let reader = self.reader();
            for (k, expr) in map {
                props.insert(k.clone(), reader.eval(expr, row)?);
            }
        }
        // AGE graphid allocation: (label_id << 48) | per-label sequence.
        let eid = self.store.allocate_edge_id(&label, &self.graph);
        let edge = Edge {
            edge_id: eid,
            source_id: src_id,
            target_id: tgt_id,
            label,
            properties: props,
        };
        self.store.add_edge(edge.clone(), &self.graph);
        Ok(edge)
    }

    // -----------------------------------------------------------------
    // SET
    // -----------------------------------------------------------------

    fn exec_set(
        &mut self,
        clause: &SetClause,
        bindings: Vec<BindingRow>,
    ) -> Result<Vec<BindingRow>, CypherError> {
        let mut next = Vec::with_capacity(bindings.len());
        for mut row in bindings {
            for item in &clause.items {
                self.apply_set_item(item, &mut row)?;
            }
            next.push(row);
        }
        Ok(next)
    }

    fn apply_set_item(&mut self, item: &SetItem, row: &mut BindingRow) -> Result<(), CypherError> {
        let value = self.reader().eval(&item.value, row)?;
        match &item.target {
            CypherExpr::PropertyAccess(PropertyAccess { variable, keys }) => {
                let Some(binding) = row.get(variable).cloned() else {
                    return Err(CypherError::UndefinedVariable(variable.clone()));
                };
                match binding {
                    Binding::Vertex(vertex) => {
                        let mut new_props = vertex.properties.clone();
                        apply_property_update(&mut new_props, keys, &value, item.operator);
                        let updated = Vertex {
                            vertex_id: vertex.vertex_id,
                            label: vertex.label.clone(),
                            properties: new_props,
                        };
                        self.store.add_vertex(updated.clone(), &self.graph);
                        row.insert(variable.clone(), Binding::Vertex(updated));
                    }
                    Binding::Edge(edge) => {
                        let mut new_props = edge.properties.clone();
                        apply_property_update(&mut new_props, keys, &value, item.operator);
                        let updated = Edge {
                            edge_id: edge.edge_id,
                            source_id: edge.source_id,
                            target_id: edge.target_id,
                            label: edge.label.clone(),
                            properties: new_props,
                        };
                        self.store.add_edge(updated.clone(), &self.graph);
                        row.insert(variable.clone(), Binding::Edge(updated));
                    }
                    _ => {
                        return Err(CypherError::TypeError(format!(
                            "SET target {variable:?} is not a vertex or edge"
                        )));
                    }
                }
            }
            CypherExpr::Variable(Variable { name }) => {
                // `SET n = {props}` or `SET n += {props}`.
                let Value::Map(replacement) = value else {
                    return Err(CypherError::TypeError(
                        "SET <var> = <expr> requires a map RHS".into(),
                    ));
                };
                let Some(binding) = row.get(name).cloned() else {
                    return Err(CypherError::UndefinedVariable(name.clone()));
                };
                if let Binding::Vertex(vertex) = binding {
                    let mut new_props = match item.operator {
                        SetOperator::Assign => BTreeMap::new(),
                        SetOperator::Update => vertex.properties.clone(),
                    };
                    new_props.extend(replacement);
                    let updated = Vertex {
                        vertex_id: vertex.vertex_id,
                        label: vertex.label.clone(),
                        properties: new_props,
                    };
                    self.store.add_vertex(updated.clone(), &self.graph);
                    row.insert(name.clone(), Binding::Vertex(updated));
                } else {
                    return Err(CypherError::TypeError(format!(
                        "SET {name:?} target must be a vertex"
                    )));
                }
            }
            other => {
                return Err(CypherError::Unsupported(format!("SET target {other:?}")));
            }
        }
        Ok(())
    }

    // -----------------------------------------------------------------
    // DELETE / DETACH DELETE
    // -----------------------------------------------------------------

    fn exec_delete(
        &mut self,
        clause: &DeleteClause,
        bindings: Vec<BindingRow>,
    ) -> Result<Vec<BindingRow>, CypherError> {
        let mut to_delete_vertices: Vec<VertexId> = Vec::new();
        let mut to_delete_edges: Vec<EdgeId> = Vec::new();
        for row in &bindings {
            for expr in &clause.expressions {
                let CypherExpr::Variable(Variable { name }) = expr else {
                    return Err(CypherError::Unsupported(
                        "DELETE only supports bare variable references".into(),
                    ));
                };
                let Some(binding) = row.get(name) else {
                    return Err(CypherError::UndefinedVariable(name.clone()));
                };
                match binding {
                    Binding::Vertex(v) => to_delete_vertices.push(v.vertex_id),
                    Binding::Edge(e) => to_delete_edges.push(e.edge_id),
                    _ => {
                        return Err(CypherError::TypeError(format!(
                            "DELETE {name:?} is not a vertex or edge"
                        )));
                    }
                }
            }
        }
        // Edges first to avoid double-delete via vertex DETACH paths.
        to_delete_edges.sort_unstable();
        to_delete_edges.dedup();
        for eid in &to_delete_edges {
            self.store.remove_edge(*eid, &self.graph);
        }
        to_delete_vertices.sort_unstable();
        to_delete_vertices.dedup();
        for vid in &to_delete_vertices {
            if !clause.detach {
                let has_out = !self.store.out_edge_ids(*vid, &self.graph).is_empty();
                let has_in = !self.store.in_edge_ids(*vid, &self.graph).is_empty();
                if has_out || has_in {
                    return Err(CypherError::TypeError(format!(
                        "cannot delete vertex {vid}: has incident edges, use DETACH DELETE"
                    )));
                }
            }
            self.store.remove_vertex(*vid, &self.graph);
        }
        Ok(bindings)
    }

    // -----------------------------------------------------------------
    // MERGE
    // -----------------------------------------------------------------

    fn exec_merge(
        &mut self,
        clause: &MergeClause,
        bindings: Vec<BindingRow>,
    ) -> Result<Vec<BindingRow>, CypherError> {
        let mut next: Vec<BindingRow> = Vec::new();
        for row in bindings {
            let matches = self.reader().match_path_pattern(&clause.pattern, &row)?;
            if matches.is_empty() {
                let mut row = row;
                self.create_path(&clause.pattern, &mut row)?;
                if let Some(items) = &clause.on_create_set {
                    for item in items {
                        self.apply_set_item(item, &mut row)?;
                    }
                }
                next.push(row);
            } else {
                for mut matched in matches {
                    if let Some(items) = &clause.on_match_set {
                        for item in items {
                            self.apply_set_item(item, &mut matched)?;
                        }
                    }
                    next.push(matched);
                }
            }
        }
        Ok(next)
    }

    // -----------------------------------------------------------------
    // UNWIND
    // -----------------------------------------------------------------

    fn exec_unwind(
        &mut self,
        clause: &UnwindClause,
        bindings: Vec<BindingRow>,
    ) -> Result<Vec<BindingRow>, CypherError> {
        let mut next = Vec::new();
        for row in bindings {
            let value = self.reader().eval(&clause.expr, &row)?;
            // AGE semantics: lists spread one row per element, null
            // yields no rows, any other scalar passes through as a
            // single row.
            let items = match value {
                Value::List(items) => items,
                Value::Null => continue,
                other => vec![other],
            };
            for item in items {
                let mut new_row = row.clone();
                new_row.insert(clause.variable.clone(), Binding::Value(item));
                next.push(new_row);
            }
        }
        Ok(next)
    }
}

fn apply_property_update(
    props: &mut BTreeMap<String, Value>,
    keys: &[String],
    value: &Value,
    op: SetOperator,
) {
    if keys.len() == 1 {
        let key = keys[0].clone();
        match (op, value) {
            (SetOperator::Update, Value::Map(rhs)) => {
                let mut existing = match props.remove(&key) {
                    Some(Value::Map(m)) => m,
                    _ => BTreeMap::new(),
                };
                existing.extend(rhs.clone());
                props.insert(key, Value::Map(existing));
            }
            _ => {
                props.insert(key, value.clone());
            }
        }
        return;
    }
    // Nested path: descend, creating maps as needed.
    let mut cursor = props;
    for key in &keys[..keys.len() - 1] {
        let entry = cursor
            .entry(key.clone())
            .or_insert_with(|| Value::Map(BTreeMap::new()));
        if !matches!(entry, Value::Map(_)) {
            *entry = Value::Map(BTreeMap::new());
        }
        if let Value::Map(inner) = entry {
            cursor = inner;
        } else {
            unreachable!();
        }
    }
    let leaf = keys.last().unwrap().clone();
    cursor.insert(leaf, value.clone());
}
