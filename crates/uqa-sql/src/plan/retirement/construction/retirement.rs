//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A detached discarded query retains its own admitted frames while the original context remains available for parent and sibling destruction.

use super::{Frame, Node, RetainedTree};
use crate::{catalog::work::Work, plan::QueryPlan, SQLError};
use uqa_core::memory::ProductionControl;

pub(in crate::plan) struct Retirement(RetainedTree<Frame>);

impl Retirement {
    pub(super) fn new(capacity: usize, control: &ProductionControl<'_>) -> Result<Self, SQLError> {
        let mut tree = RetainedTree::with_budget(control.budget());
        tree.frames.reserve(capacity.max(1))?;
        Ok(Self(tree))
    }

    /// Transfer the payload before creating or polling any fallible cleanup future.
    pub(in crate::plan) fn query(&mut self, query: QueryPlan) {
        assert!(matches!(self.0.root, Node::Empty) && self.0.frames.is_empty());
        self.0.root = Node::Query(query);
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
            if self.0.advance() {
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests;
