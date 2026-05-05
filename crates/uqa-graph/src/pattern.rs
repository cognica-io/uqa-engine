//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Declarative graph patterns (Definition 5.2.1, Paper 2):
//! `P = (V_P, E_P, C_V, C_E)`.
//!
//! Constraints are encoded as `VertexPredicate` / `EdgePredicate`
//! enums rather than open closures so patterns are serializable and
//! introspectable by the planner. A `Custom` arm exists for callers
//! that need an escape hatch.

use std::sync::Arc;

use uqa_core::{Edge, Value, Vertex};

/// Predicate over a vertex.
#[derive(Clone)]
pub enum VertexPredicate {
    /// `vertex.label == label`.
    LabelEq(String),
    /// `vertex.properties[key] == value`.
    PropertyEq { key: String, value: Value },
    /// Property is present (any value).
    PropertyExists(String),
    /// Conjunction of nested predicates (n-ary AND).
    All(Vec<VertexPredicate>),
    /// User-supplied predicate. Carried by `Arc` so the pattern stays
    /// `Clone`; the closure is shared by reference across clones.
    Custom(Arc<dyn Fn(&Vertex) -> bool + Send + Sync>),
}

impl VertexPredicate {
    pub fn matches(&self, vertex: &Vertex) -> bool {
        match self {
            VertexPredicate::LabelEq(l) => vertex.label == *l,
            VertexPredicate::PropertyEq { key, value } => vertex.properties.get(key) == Some(value),
            VertexPredicate::PropertyExists(key) => vertex.properties.contains_key(key),
            VertexPredicate::All(preds) => preds.iter().all(|p| p.matches(vertex)),
            VertexPredicate::Custom(f) => f(vertex),
        }
    }
}

impl std::fmt::Debug for VertexPredicate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VertexPredicate::LabelEq(l) => write!(f, "LabelEq({l:?})"),
            VertexPredicate::PropertyEq { key, value } => {
                write!(f, "PropertyEq({key:?} = {value:?})")
            }
            VertexPredicate::PropertyExists(k) => write!(f, "PropertyExists({k:?})"),
            VertexPredicate::All(ps) => f.debug_tuple("All").field(ps).finish(),
            VertexPredicate::Custom(_) => write!(f, "Custom(<fn>)"),
        }
    }
}

/// Predicate over an edge.
#[derive(Clone)]
pub enum EdgePredicate {
    /// `edge.properties[key] == value`.
    PropertyEq {
        key: String,
        value: Value,
    },
    PropertyExists(String),
    All(Vec<EdgePredicate>),
    Custom(Arc<dyn Fn(&Edge) -> bool + Send + Sync>),
}

impl EdgePredicate {
    pub fn matches(&self, edge: &Edge) -> bool {
        match self {
            EdgePredicate::PropertyEq { key, value } => edge.properties.get(key) == Some(value),
            EdgePredicate::PropertyExists(key) => edge.properties.contains_key(key),
            EdgePredicate::All(preds) => preds.iter().all(|p| p.matches(edge)),
            EdgePredicate::Custom(f) => f(edge),
        }
    }
}

impl std::fmt::Debug for EdgePredicate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EdgePredicate::PropertyEq { key, value } => {
                write!(f, "PropertyEq({key:?} = {value:?})")
            }
            EdgePredicate::PropertyExists(k) => write!(f, "PropertyExists({k:?})"),
            EdgePredicate::All(ps) => f.debug_tuple("All").field(ps).finish(),
            EdgePredicate::Custom(_) => write!(f, "Custom(<fn>)"),
        }
    }
}

/// `(V_P)` — a vertex variable in a pattern, with optional constraints.
#[derive(Debug, Clone)]
pub struct VertexPattern {
    pub variable: String,
    pub constraints: Vec<VertexPredicate>,
}

impl VertexPattern {
    pub fn new(variable: impl Into<String>) -> Self {
        Self {
            variable: variable.into(),
            constraints: Vec::new(),
        }
    }

    pub fn with(mut self, predicate: VertexPredicate) -> Self {
        self.constraints.push(predicate);
        self
    }

    pub fn satisfies(&self, vertex: &Vertex) -> bool {
        self.constraints.iter().all(|c| c.matches(vertex))
    }
}

/// `(E_P)` — an edge between two vertex variables, with an optional
/// label and per-property constraints. `negated == true` flips it into
/// a "must not exist" pattern, matched after positive edges resolve.
#[derive(Debug, Clone)]
pub struct EdgePattern {
    pub source_var: String,
    pub target_var: String,
    pub label: Option<String>,
    pub constraints: Vec<EdgePredicate>,
    pub negated: bool,
}

impl EdgePattern {
    pub fn new(source: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            source_var: source.into(),
            target_var: target.into(),
            label: None,
            constraints: Vec::new(),
            negated: false,
        }
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn with(mut self, predicate: EdgePredicate) -> Self {
        self.constraints.push(predicate);
        self
    }

    pub fn negated(mut self) -> Self {
        self.negated = true;
        self
    }

    pub fn satisfies(&self, edge: &Edge) -> bool {
        if let Some(label) = &self.label {
            if edge.label != *label {
                return false;
            }
        }
        self.constraints.iter().all(|c| c.matches(edge))
    }
}

/// Subgraph pattern: `P = (V_P, E_P, C_V, C_E)`.
#[derive(Debug, Clone, Default)]
pub struct GraphPattern {
    pub vertex_patterns: Vec<VertexPattern>,
    pub edge_patterns: Vec<EdgePattern>,
}

impl GraphPattern {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_vertex(mut self, vp: VertexPattern) -> Self {
        self.vertex_patterns.push(vp);
        self
    }

    pub fn add_edge(mut self, ep: EdgePattern) -> Self {
        self.edge_patterns.push(ep);
        self
    }
}
