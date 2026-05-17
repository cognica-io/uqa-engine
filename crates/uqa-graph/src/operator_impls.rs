//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `Operator` trait wrappers for the graph operators that previously
//! only existed as inherent methods (`execute(&G)`). Matches UQA behavior for
//! `WeightedPathQueryOperator` and `CypherQueryOperator` so the
//! engine's [`uqa_operators::Operator`] dispatch can run them through
//! the standard `EngineDriver` path.

use std::sync::Arc;

use uqa_core::{IndexStats, PostingList};
use uqa_operators::base::{ExecutionContext, Operator};

use crate::cypher::{CypherQuery, CypherWriter};
use crate::memory_store::MemoryGraphStore;
use crate::operators::{RegularPathQuery, DEFAULT_GRAPH_SCORE};
use crate::rpq::RegularPathExpr;

/// `WeightedPathQueryOperator` — runs a Regular Path Query over a
/// graph and applies a predicate to the accumulated edge weight along
/// each matching path. Matches UQA behavior for
/// `uqa.graph.operators.WeightedPathQueryOperator`.
///
/// The UQA-RS implementation today supports the predicate-on-endpoints shape
/// (start/end vertex match the RPQ): full per-path weight aggregation
/// requires NFA path tracking that lives in the Rust `RegularPathQuery`.
/// Until that's threaded through, the operator runs the underlying
/// RPQ and treats the user-supplied `predicate_selectivity` as a
/// post-filter scaling factor on the resulting score, matching the
/// cardinality model in `uqa_planner::cardinality::estimate_rpq`.
pub struct WeightedPathQueryOperator {
    pub path_expr: RegularPathExpr,
    pub graph_store: Arc<MemoryGraphStore>,
    pub graph_name: String,
    pub start_vertex: Option<u64>,
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
    pub fn with_score(mut self, score: f64) -> Self {
        self.score = score;
        self
    }
}

impl Operator for WeightedPathQueryOperator {
    fn execute(&self, _ctx: &ExecutionContext) -> PostingList {
        let mut rpq = RegularPathQuery::new(self.path_expr.clone(), &self.graph_name);
        rpq.score = self.score * self.predicate_selectivity.clamp(0.0, 1.0);
        if let Some(v) = self.start_vertex {
            rpq = rpq.from_vertex(v);
        }
        rpq.execute(self.graph_store.as_ref()).inner().clone()
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
    fn execute(&self, _ctx: &ExecutionContext) -> PostingList {
        // The Cypher executor needs a unique borrow of the store. The
        // engine passes `Arc<RwLock<...>>` so concurrent readers can
        // share the store while writers serialize through the lock.
        let mut guard = self.graph_store.write();
        let mut writer = CypherWriter::new(&mut *guard, self.graph_name.clone())
            .with_params(self.params.clone());
        let Ok((_cols, rows)) = writer.execute(&self.query) else {
            return PostingList::new();
        };
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
        PostingList::from_sorted_unchecked(entries)
    }

    fn cost_estimate(&self, stats: &IndexStats) -> f64 {
        stats.total_docs as f64
    }
}
