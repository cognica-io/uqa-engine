//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Boolean operators: union, intersect, complement.

use std::sync::Arc;

use uqa_core::{IndexStats, Payload, PostingEntry, PostingList};

use crate::base::{missing_backend, ExecutionContext, Operator, OperatorResult};

pub struct UnionOperator {
    pub operands: Vec<Arc<dyn Operator>>,
}

impl UnionOperator {
    pub fn new(operands: Vec<Arc<dyn Operator>>) -> Self {
        Self { operands }
    }
}

impl Operator for UnionOperator {
    fn execute(&self, ctx: &ExecutionContext) -> OperatorResult {
        let mut result = PostingList::new();
        for operand in &self.operands {
            result = result.union(&operand.execute(ctx)?);
        }
        Ok(result)
    }

    fn cost_estimate(&self, stats: &IndexStats) -> f64 {
        self.operands.iter().map(|op| op.cost_estimate(stats)).sum()
    }
}

pub struct IntersectOperator {
    pub operands: Vec<Arc<dyn Operator>>,
}

impl IntersectOperator {
    pub fn new(operands: Vec<Arc<dyn Operator>>) -> Self {
        Self { operands }
    }
}

impl Operator for IntersectOperator {
    fn execute(&self, ctx: &ExecutionContext) -> OperatorResult {
        let mut iter = self.operands.iter();
        let Some(first) = iter.next() else {
            return Ok(PostingList::new());
        };
        let mut acc = first.execute(ctx)?;
        for op in iter {
            if acc.is_empty() {
                return Ok(acc);
            }
            acc = acc.intersect_owned(&op.execute(ctx)?);
        }
        Ok(acc)
    }

    fn cost_estimate(&self, stats: &IndexStats) -> f64 {
        self.operands
            .iter()
            .map(|op| op.cost_estimate(stats))
            .fold(f64::INFINITY, f64::min)
            .max(0.0)
    }
}

/// Complement with respect to the universal set drawn from
/// `ctx.document_store.doc_ids()`.
pub struct ComplementOperator {
    pub operand: Arc<dyn Operator>,
}

impl ComplementOperator {
    pub fn new(operand: Arc<dyn Operator>) -> Self {
        Self { operand }
    }
}

impl Operator for ComplementOperator {
    fn execute(&self, ctx: &ExecutionContext) -> OperatorResult {
        let result = self.operand.execute(ctx)?;
        let Some(doc_store) = ctx.document_store.as_ref() else {
            return Err(missing_backend("document-store", "boolean complement"));
        };
        let universal_entries: Vec<PostingEntry> = doc_store
            .doc_ids()?
            .into_iter()
            .map(|id| PostingEntry::new(id, Payload::default()))
            .collect();
        let universal = PostingList::from_sorted_unchecked(universal_entries);
        Ok(result.complement(&universal))
    }

    fn cost_estimate(&self, stats: &IndexStats) -> f64 {
        stats.total_docs as f64
    }
}
