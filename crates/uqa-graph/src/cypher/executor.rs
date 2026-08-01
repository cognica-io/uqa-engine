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
}

mod expression;
mod functions;
mod helpers;
mod matching;
mod projection;

use helpers::{
    aggregate_avg, aggregate_extreme, aggregate_sum, agtype_add, agtype_div, agtype_mod,
    agtype_pow, domain_float_fn, float_fn, is_aggregate, is_aggregate_name, null_or_bool,
    numeric_op, pattern_variables, regex_match, return_label, sort_keyed, str_predicate, string_fn,
    unsupported_argument, validated_path_elements,
};
