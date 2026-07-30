//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `Operator` trait wrappers for graph operators that previously only
//! existed as inherent methods (`execute(&G)`). They support generic
//! [`uqa_operators::Operator`] composition. The engine's exhaustive
//! `OperatorTree` driver invokes the corresponding graph primitives against
//! its named graph store; SQL functions represented by those IR nodes use the
//! same optimized physical path.

use std::sync::Arc;

use uqa_core::{IndexStats, PostingList};
use uqa_operators::base::{ExecutionContext, Operator, OperatorResult};
use uqa_operators::PathWeightPredicate;
use uqa_storage::StorageBackendError;

use crate::cypher::{CypherQuery, CypherWriter};
use crate::memory_store::MemoryGraphStore;
use crate::operators::{WeightedPathQuery, DEFAULT_GRAPH_SCORE};
use crate::rpq::RegularPathExpr;

/// `WeightedPathQueryOperator` evaluates bounded DFA walks, sums a numeric
/// edge property, and keeps endpoints for which the accumulated weight passes
/// `predicate`. The selectivity value is retained solely for planning; it
/// never substitutes for physical predicate evaluation.
pub struct WeightedPathQueryOperator {
    pub path_expr: RegularPathExpr,
    pub graph_store: Arc<MemoryGraphStore>,
    pub graph_name: String,
    pub start_vertex: Option<u64>,
    pub weight_property: String,
    pub default_edge_weight: f64,
    pub max_hops: usize,
    pub predicate: PathWeightPredicate,
    pub predicate_selectivity: f64,
    pub score: f64,
}

impl WeightedPathQueryOperator {
    #[must_use]
    pub fn new(
        path_expr: RegularPathExpr,
        graph_store: Arc<MemoryGraphStore>,
        graph_name: impl Into<String>,
    ) -> Self {
        Self {
            path_expr,
            graph_store,
            graph_name: graph_name.into(),
            start_vertex: None,
            weight_property: "weight".to_string(),
            default_edge_weight: 1.0,
            max_hops: 16,
            predicate: Arc::new(|_| true),
            predicate_selectivity: 1.0,
            score: DEFAULT_GRAPH_SCORE,
        }
    }

    #[must_use]
    pub fn from_vertex(mut self, start: u64) -> Self {
        self.start_vertex = Some(start);
        self
    }

    #[must_use]
    pub fn with_predicate_selectivity(mut self, sel: f64) -> Self {
        self.predicate_selectivity = sel;
        self
    }

    #[must_use]
    pub fn with_predicate(
        mut self,
        predicate: impl Fn(f64) -> bool + Send + Sync + 'static,
        selectivity: f64,
    ) -> Self {
        self.predicate = Arc::new(predicate);
        self.predicate_selectivity = selectivity;
        self
    }

    #[must_use]
    pub fn with_weight_property(mut self, property: impl Into<String>) -> Self {
        self.weight_property = property.into();
        self
    }

    #[must_use]
    pub fn with_default_edge_weight(mut self, weight: f64) -> Self {
        self.default_edge_weight = weight;
        self
    }

    #[must_use]
    pub fn with_max_hops(mut self, max_hops: usize) -> Self {
        self.max_hops = max_hops;
        self
    }

    #[must_use]
    pub fn with_score(mut self, score: f64) -> Self {
        self.score = score;
        self
    }
}

impl Operator for WeightedPathQueryOperator {
    fn execute(&self, _ctx: &ExecutionContext) -> OperatorResult {
        let mut query = WeightedPathQuery::new(
            self.path_expr.clone(),
            &self.graph_name,
            &self.weight_property,
            Arc::clone(&self.predicate),
        );
        query.default_edge_weight = self.default_edge_weight;
        query.max_hops = self.max_hops;
        query.score = self.score;
        if let Some(v) = self.start_vertex {
            query = query.from_vertex(v);
        }
        Ok(query
            .execute(self.graph_store.as_ref())
            .map_err(|error| StorageBackendError::Other(error.to_string()))?
            .to_posting_list())
    }

    fn cost_estimate(&self, stats: &IndexStats) -> f64 {
        // O(V^2 * |R|) approximation; the exact cardinality model
        // lives in `uqa_planner::cardinality::estimate_rpq`. This
        // stays a coarse fallback so the trait impl is self-contained.
        let n = stats.total_docs as f64;
        n * n
    }
}

/// `CypherQueryOperator` — execute a parsed openCypher query against
/// a named graph and project the resulting `(start, end)` vertex
/// pairs into a posting list. Matches UQA behavior for
/// `uqa.graph.operators.CypherQueryOperator`.
///
/// The UQA-RS implementation routes through [`CypherWriter`] (the mutating
/// executor) since it covers the full clause set including
/// CREATE/MERGE/SET/DELETE/UNWIND. Read-only queries still flow
/// through the same path; the writer leaves the store untouched
/// when the query is read-only.
pub struct CypherQueryOperator {
    pub graph_store: Arc<parking_lot::RwLock<MemoryGraphStore>>,
    pub query: CypherQuery,
    pub graph_name: String,
    pub params: std::collections::BTreeMap<String, uqa_core::Value>,
}

impl CypherQueryOperator {
    #[must_use]
    pub fn new(
        graph_store: Arc<parking_lot::RwLock<MemoryGraphStore>>,
        query: CypherQuery,
        graph_name: impl Into<String>,
    ) -> Self {
        Self {
            graph_store,
            query,
            graph_name: graph_name.into(),
            params: std::collections::BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn with_params(
        mut self,
        params: std::collections::BTreeMap<String, uqa_core::Value>,
    ) -> Self {
        self.params = params;
        self
    }
}

impl Operator for CypherQueryOperator {
    fn execute(&self, _ctx: &ExecutionContext) -> OperatorResult {
        // The Cypher executor needs a unique borrow of the store. The
        // engine passes `Arc<RwLock<...>>` so concurrent readers can
        // share the store while writers serialize through the lock.
        let mut guard = self.graph_store.write();
        let mut writer = CypherWriter::new(&mut *guard, self.graph_name.clone())
            .with_params(self.params.clone());
        let (_cols, rows) = writer
            .execute(&self.query)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        // Project bound vertex/edge ids out of the result rows. The
        // posting list carries one entry per distinct vertex id seen,
        // so downstream operators can intersect / union the result
        // against any other graph-result set.
        let mut ids: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
        for row in rows {
            for value in row.values() {
                if let uqa_core::Value::Int(n) = value {
                    if *n >= 0 {
                        ids.insert(*n as u64);
                    }
                }
            }
        }
        let entries: Vec<uqa_core::PostingEntry> = ids
            .into_iter()
            .map(|id| uqa_core::PostingEntry::new(id, uqa_core::Payload::default()))
            .collect();
        Ok(PostingList::from_sorted_unchecked(entries))
    }

    fn cost_estimate(&self, stats: &IndexStats) -> f64 {
        stats.total_docs as f64
    }
}
