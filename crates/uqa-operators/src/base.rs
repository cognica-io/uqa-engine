//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `Operator` trait, [`ExecutionContext`] holding storage backends, and
//! the monoidal [`ComposedOperator`].

use std::sync::Arc;

use uqa_core::{IndexStats, PostingList};
use uqa_storage::{DocumentStore, InvertedIndex};

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
    pub stats: Option<IndexStats>,
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

    pub fn with_stats(mut self, stats: IndexStats) -> Self {
        self.stats = Some(stats);
        self
    }
}

pub trait Operator: Send + Sync {
    fn execute(&self, ctx: &ExecutionContext) -> PostingList;

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
    fn execute(&self, ctx: &ExecutionContext) -> PostingList {
        let mut result = PostingList::new();
        for op in &self.operands {
            result = op.execute(ctx);
        }
        result
    }

    fn cost_estimate(&self, stats: &IndexStats) -> f64 {
        self.operands.iter().map(|op| op.cost_estimate(stats)).sum()
    }
}
