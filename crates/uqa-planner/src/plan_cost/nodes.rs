//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain each estimate from the original lexical traversal without replanning children.

use super::{CommandPlan, PlanCost, QueryPlan, SourcePlan};
use std::cell::{Cell, RefCell};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};
use uqa_sql::SQLError;

mod order;

/// Borrow the exact input whose cost was computed; callers never reconstruct its scope from a display name.
#[derive(Debug, Clone, Copy)]
pub enum PlanCostInput<'a> {
    Query(&'a QueryPlan),
    Source(&'a SourcePlan),
    Command(&'a CommandPlan),
}

/// Parent indices precede their children. Costs describe one invocation before an enclosing consumer applies its multiplicity.
#[derive(Debug, Clone, Copy)]
pub struct PlanCostNode<'a> {
    pub parent: Option<usize>,
    pub input: PlanCostInput<'a>,
    pub cost: PlanCost,
}

/// The admitted buffer borrows immutable plan inputs and owns its complete retention allowance.
#[derive(Debug)]
pub struct PlanCostTree<'a> {
    pub root: PlanCost,
    pub nodes: Produced<Vec<PlanCostNode<'a>>>,
}

pub(super) struct Capture<'p, 'c> {
    nodes: RefCell<ProductionVec<'c, PlanCostNode<'p>>>,
    parent: Cell<Option<usize>>,
    control: ProductionControl<'c>,
}

impl<'p, 'c> Capture<'p, 'c> {
    pub(super) fn new(control: ProductionControl<'c>) -> Self {
        Self {
            nodes: RefCell::new(ProductionVec::new(control)),
            parent: Cell::new(None),
            control,
        }
    }

    pub(super) fn finish(self, root: PlanCost) -> Result<PlanCostTree<'p>, SQLError> {
        Ok(PlanCostTree {
            root,
            nodes: order::preorder(self.nodes.into_inner().finish()?, self.control)?,
        })
    }

    pub(super) fn parent(capture: Option<&Self>) -> Option<usize> {
        capture.and_then(|capture| capture.parent.get())
    }

    pub(super) fn at_parent(
        capture: Option<&Self>,
        parent: Option<usize>,
        estimate: impl FnOnce() -> Result<PlanCost, SQLError>,
    ) -> Result<PlanCost, SQLError> {
        let Some(capture) = capture else {
            return estimate();
        };
        let previous = capture.parent.replace(parent);
        let _scope = ParentScope {
            parent: &capture.parent,
            previous,
        };
        estimate()
    }

    pub(super) fn node(
        capture: Option<&Self>,
        input: PlanCostInput<'p>,
        estimate: impl FnOnce() -> Result<PlanCost, SQLError>,
    ) -> Result<PlanCost, SQLError> {
        let Some(capture) = capture else {
            return estimate();
        };
        capture.control.check()?;
        let parent = capture.parent.get();
        let index = {
            let mut nodes = capture.nodes.borrow_mut();
            let index = nodes.len();
            nodes.push_copy(PlanCostNode {
                parent,
                input,
                cost: PlanCost::default(),
            })?;
            index
        };
        capture.parent.set(Some(index));
        let _scope = ParentScope {
            parent: &capture.parent,
            previous: parent,
        };
        let cost = estimate()?;
        capture.control.check()?;
        capture.nodes.borrow_mut().as_mut_slice()[index].cost = cost;
        Ok(cost)
    }
}

struct ParentScope<'a> {
    parent: &'a Cell<Option<usize>>,
    previous: Option<usize>,
}

impl Drop for ParentScope<'_> {
    fn drop(&mut self) {
        self.parent.set(self.previous);
    }
}

#[cfg(test)]
mod tests;
