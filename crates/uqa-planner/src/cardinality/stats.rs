//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relational, graph, and sampling statistics contracts.

use super::{BTreeMap, Value, VertexConstraint};

/// Jaccard-style selectivity assumed for text-similarity joins when no
/// per-column statistics are available.
pub const JACCARD_JOIN_SELECTIVITY: f64 = 0.05;

/// Default vector-similarity join selectivity.
pub const VECTOR_JOIN_SELECTIVITY: f64 = 0.1;

/// Fallback average out-degree used by graph traversal cardinality
/// when no [`GraphStats`] is supplied.
pub const GRAPH_AVG_DEGREE_DEFAULT: f64 = 10.0;

// ---------------------------------------------------------------------
// Graph statistics
// ---------------------------------------------------------------------

/// Graph-level statistics for heuristic cardinality estimation.
#[derive(Debug, Clone, Default)]
pub struct GraphStats {
    pub num_vertices: u64,
    pub num_edges: u64,
    pub label_counts: BTreeMap<String, u64>,
    pub avg_out_degree: f64,
    pub degree_distribution: BTreeMap<u64, u64>,
    pub min_timestamp: Option<f64>,
    pub max_timestamp: Option<f64>,
    pub graph_name: String,
    pub vertex_label_counts: BTreeMap<String, u64>,
    pub label_degree_map: BTreeMap<String, f64>,
}

impl GraphStats {
    /// Fraction of edges matching `label`. `None` is the wildcard label
    /// (full edge population).
    pub fn label_selectivity(&self, label: Option<&str>) -> f64 {
        match label {
            None => 1.0,
            Some(_) if self.num_edges == 0 => 1.0,
            Some(name) => {
                let c = self.label_counts.get(name).copied().unwrap_or(0);
                c as f64 / self.num_edges as f64
            }
        }
    }

    /// Edge density `|E| / |V|^2`.
    pub fn edge_density(&self) -> f64 {
        if self.num_vertices <= 1 {
            return 0.0;
        }
        let nv = self.num_vertices as f64;
        self.num_edges as f64 / (nv * nv)
    }
}

// ---------------------------------------------------------------------
// Random-walk sampler trait used by `_sample_graph_cardinality`.
// ---------------------------------------------------------------------

/// One outgoing edge surfaced by a [`GraphStoreSampler`].
pub struct EdgeSample {
    pub target_id: u64,
    pub label: String,
}

/// Minimal graph-store interface exposing the vertex, adjacency, and edge
/// snapshots required by the sampler.
pub trait GraphStoreSampler: Send + Sync {
    /// IDs of every vertex in the store.
    fn vertex_ids(&self) -> Vec<u64>;

    /// Outgoing edges from `vid`.
    fn outgoing_edges(&self, vid: u64) -> Vec<EdgeSample>;

    /// Apply a vertex-constraint callback so the sampler can keep vertex
    /// storage behind the store implementation.
    fn vertex_satisfies(&self, vid: u64, constraint: &VertexConstraint) -> bool;
}

// ---------------------------------------------------------------------
// Per-column statistics (used by both AST-Expr and operator surfaces).
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct ColumnStats {
    pub distinct_count: u64,
    pub null_count: u64,
    pub min_value: Option<Value>,
    pub max_value: Option<Value>,
    pub row_count: u64,
    /// Equi-depth histogram bucket boundaries, sorted ascending.
    /// `b+1` boundaries describe `b` buckets.
    pub histogram: Vec<Value>,
    /// Most-common values, descending by frequency.
    pub mcv_values: Vec<Value>,
    pub mcv_frequencies: Vec<f64>,
}

impl ColumnStats {
    /// Default selectivity of an equality predicate over this column.
    pub fn equality_selectivity(&self) -> f64 {
        if self.distinct_count == 0 {
            1.0
        } else {
            1.0 / self.distinct_count as f64
        }
    }

    pub fn matches_mcv(&self, value: &Value) -> Option<f64> {
        for (mcv, freq) in self.mcv_values.iter().zip(self.mcv_frequencies.iter()) {
            if mcv == value {
                return Some(*freq);
            }
        }
        None
    }
}

#[derive(Debug, Clone, Default)]
pub struct RelationStats {
    pub row_count: u64,
    pub columns: BTreeMap<String, ColumnStats>,
}

impl RelationStats {
    pub fn new(row_count: u64) -> Self {
        Self {
            row_count,
            columns: BTreeMap::new(),
        }
    }

    pub fn with_column(mut self, name: impl Into<String>, stats: ColumnStats) -> Self {
        self.columns.insert(name.into(), stats);
        self
    }

    pub fn column(&self, name: &str) -> Option<&ColumnStats> {
        self.columns.get(name)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Selectivity(pub f64);

impl Selectivity {
    pub fn clamp(self) -> Self {
        Self(self.0.clamp(0.0, 1.0))
    }

    pub fn raw(self) -> f64 {
        self.0
    }
}
