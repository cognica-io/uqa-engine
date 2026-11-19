//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A detached discarded query retains its own admitted frames while the original context remains available for parent and sibling destruction.

use super::{Frame, Local, Node, RetainedTree};
use crate::{
    catalog::work::Work,
    plan::{QueryPlan, SourcePlan},
    SQLError,
};
use uqa_core::memory::{MemoryReservation, ProductionControl};

pub(in crate::plan) struct Retirement {
    tree: Box<RetainedTree<Frame<Local>>>,
    // The detached workspace retires its payload and storage before returning its fixed allocation.
    _memory: Option<MemoryReservation>,
}

impl Retirement {
    pub(super) fn new(capacity: usize, control: &ProductionControl<'_>) -> Result<Self, SQLError> {
        let memory = control.reserve(size_of::<RetainedTree<Frame<Local>>>())?;
        let mut tree = Box::new(RetainedTree::with_budget(control.budget()));
        tree.frames.reserve(capacity.max(1))?;
        control.check()?;
        Ok(Self {
            tree,
            _memory: memory,
        })
    }

    /// Transfer the payload before creating or polling any fallible cleanup future.
    pub(in crate::plan) fn query(&mut self, query: QueryPlan) {
        self.node(Node::Query(query));
    }

    pub(in crate::plan) fn source(&mut self, source: SourcePlan) {
        self.node(Node::Source(source));
    }

    fn node(&mut self, node: Node) {
        assert!(matches!(self.tree.root, Node::Empty) && self.tree.frames.is_empty());
        self.tree.root = node;
    }

    pub(in crate::plan) async fn retire_from<W: Work>(
        &mut self,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<(), SQLError> {
        loop {
            control.check()?;
            work.checkpoint().await?;
            control.check()?;
            if self.tree.advance() {
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests;
