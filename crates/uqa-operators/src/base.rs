//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `Operator` trait, [`ExecutionContext`] holding storage backends, and
//! the monoidal [`ComposedOperator`].

use std::collections::BTreeMap;
use std::sync::Arc;

use uqa_core::{FieldName, IndexStats, PostingList};
use uqa_storage::{
    DocumentStore, InvertedIndex, StorageBackendError, StorageBackendResult, VectorIndex,
};

pub type OperatorResult = StorageBackendResult<PostingList>;

pub(crate) fn missing_backend(backend: &str, operation: &str) -> StorageBackendError {
    StorageBackendError::Other(format!(
        "{operation} requires an execution-context {backend} backend"
    ))
}

pub(crate) fn require_finite_score(score: f64, operation: &str) -> StorageBackendResult<()> {
    if score.is_finite() {
        Ok(())
    } else {
        Err(StorageBackendError::Other(format!(
            "{operation} received a non-finite score {score}"
        )))
    }
}

pub(crate) fn require_probability(probability: f64, operation: &str) -> StorageBackendResult<()> {
    if probability.is_finite() && (0.0..=1.0).contains(&probability) {
        Ok(())
    } else {
        Err(StorageBackendError::Other(format!(
            "{operation} requires probability scores in [0, 1], got {probability}"
        )))
    }
}

/// Storage handles passed to every operator's `execute` call.
///
/// Holding `Arc<dyn ...>` rather than borrows keeps the operator tree
/// independent of how the engine owns its stores. The trade-off is one
/// `Arc::clone` per operator at engine boundary; per-operator dispatch
/// remains a single virtual call.
#[derive(Default, Clone)]
pub struct ExecutionContext {
    pub document_store: Option<Arc<dyn DocumentStore>>,
    pub inverted_index: Option<Arc<dyn InvertedIndex>>,
    pub vector_indexes: BTreeMap<FieldName, Arc<dyn VectorIndex>>,
    pub stats: Option<IndexStats>,
    /// Optional named graph (label-only neighbor lookup) for the
    /// graph-aware deep-fusion layers (`Propagate`, `Conv`, `Pool`).
    /// Held as a generic neighbor-lookup callback so this crate stays
    /// independent of `uqa-graph`.
    pub graph: Option<Arc<dyn GraphNeighborLookup>>,
}

/// Minimal trait capturing the only graph operation deep-fusion's
/// graph layers need: enumerate the neighbors of a vertex along a
/// label in a chosen direction.
///
/// An empty `label` is the explicit wildcard and must enumerate neighbors
/// across every edge label. This lets IR nodes represent an omitted edge
/// label without inventing a separate sentinel at each engine boundary.
pub trait GraphNeighborLookup: Send + Sync {
    fn neighbors(
        &self,
        vertex: u64,
        label: &str,
        direction: Direction,
    ) -> StorageBackendResult<Vec<u64>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Out,
    In,
    Both,
}

impl ExecutionContext {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_inverted_index(mut self, idx: Arc<dyn InvertedIndex>) -> Self {
        self.inverted_index = Some(idx);
        self
    }

    pub fn with_document_store(mut self, ds: Arc<dyn DocumentStore>) -> Self {
        self.document_store = Some(ds);
        self
    }

    pub fn with_vector_index(
        mut self,
        field: impl Into<FieldName>,
        idx: Arc<dyn VectorIndex>,
    ) -> Self {
        self.vector_indexes.insert(field.into(), idx);
        self
    }

    pub fn with_stats(mut self, stats: IndexStats) -> Self {
        self.stats = Some(stats);
        self
    }

    pub fn with_graph(mut self, graph: Arc<dyn GraphNeighborLookup>) -> Self {
        self.graph = Some(graph);
        self
    }
}

pub trait Operator: Send + Sync {
    fn execute(&self, ctx: &ExecutionContext) -> OperatorResult;

    fn cost_estimate(&self, stats: &IndexStats) -> f64 {
        stats.total_docs as f64
    }
}

/// Sequential composition: the right-hand operand's result wins. Used as
/// the monoidal product for the operator monoid; the empty composition is
/// the identity.
pub struct ComposedOperator {
    pub operands: Vec<Arc<dyn Operator>>,
}

impl ComposedOperator {
    pub fn new(operands: Vec<Arc<dyn Operator>>) -> Self {
        Self { operands }
    }
}

impl Operator for ComposedOperator {
    fn execute(&self, ctx: &ExecutionContext) -> OperatorResult {
        let mut result = PostingList::new();
        for op in &self.operands {
            result = op.execute(ctx)?;
        }
        Ok(result)
    }

    fn cost_estimate(&self, stats: &IndexStats) -> f64 {
        self.operands.iter().map(|op| op.cost_estimate(stats)).sum()
    }
}
