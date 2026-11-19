//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native plan reconstruction keeps every partial node inside one admitted owner.

macro_rules! plan_scalar_list {
    ($read:ident, $owner:expr, $allowance:expr, |$input:pat_param, $memory:pat_param| $body:block) => {{
        let count = $read!($owner.binary.count())?;
        let mut buffer = uqa_core::memory::BudgetedVec::new($owner.binary.memory);
        buffer.reserve(count)?;
        let (buffer, reservation) = buffer.into_parts();
        $allowance.absorb(reservation);
        let mut values = $crate::catalog::image::input::plans::retirement::Pending::new(buffer);
        for _ in 0..count {
            $read!($owner.checkpoint())?;
            let $input = &mut *$owner;
            let $memory = &mut *$allowance;
            let value: Result<_, SQLError> = $body;
            values.push(value?.take());
        }
        Ok::<_, SQLError>(values)
    }};
}

mod api;
mod assignments;
mod ctes;
mod driver;
mod fields;
mod forest;
pub(super) use crate::plan::retirement::empty_query;
use crate::plan::retirement::{empty_source, Node};
use forest::NativeNode;
mod memory;
mod metadata;
use memory::PlanMemory;
mod mutations;
mod ownership;
mod queries;
mod retirement;
mod sources;
mod statements;
#[cfg(test)]
mod tests;
mod unified;
mod validation;
mod windows;

pub use api::{CapturedExpressionPlan, CapturedQuery};
pub use statements::CapturedStatement;
pub(in crate::catalog::image) use statements::{read_routines, read_rules};
pub use unified::CapturedPlan;

use super::{invalid, Fields, Input};
use crate::plan::retirement::RetainedTree;
use crate::{
    plan::{CommandPlan, CtePlan, CtePlanBody, ExpressionPlan, QueryPlan},
    SQLError, ScalarExpr,
};
use uqa_core::memory::MemoryBudget;

#[derive(Clone, Copy)]
pub(super) enum Kind {
    Query,
    Source,
    Cte,
    Command,
    Expression,
}

#[derive(Debug)]
pub(super) struct Frame<N = Node> {
    node: N,
    position: usize,
    count: Option<usize>,
    retiring: bool,
    references: super::scalars::References,
    rule_references: super::scalars::References,
}

impl<N> Frame<N> {
    fn new(node: N) -> Self {
        Self {
            node,
            position: 0,
            count: None,
            retiring: false,
            references: super::scalars::References::default(),
            rule_references: super::scalars::References::default(),
        }
    }
}

#[derive(Debug)]
pub(super) struct Owner<N: NativeNode = Node> {
    tree: RetainedTree<Frame<N>>,
    memory: PlanMemory,
}

impl<N: NativeNode> Owner<N> {
    fn new(memory: &MemoryBudget) -> Self {
        Self {
            tree: RetainedTree::new(memory),
            memory: PlanMemory::new(memory),
        }
    }

    fn begin(&mut self, kind: N::Kind) -> Result<(), SQLError> {
        // One extra slot covers a required, still-empty boxed child during partial reconstruction. Retirement reuses this capacity and never acquires another allowance.
        self.tree.frames.reserve(2)?;
        let node = N::new(kind, &mut self.memory)?;
        self.tree.frames.push(Frame::new(node))?;
        Ok(())
    }
}

impl Node {
    fn new(kind: Kind, memory: &mut PlanMemory) -> Result<Self, SQLError> {
        Ok(match kind {
            Kind::Query => Node::Query(empty_query()),
            Kind::Source => Node::Source(empty_source()),
            Kind::Command => Node::Command(CommandPlan::Empty),
            Kind::Expression => Node::Expression(ExpressionPlan {
                scalar: ScalarExpr::Star,
                subqueries: Vec::new(),
            }),
            Kind::Cte => {
                memory.grow(size_of::<QueryPlan>())?;
                Node::Cte(CtePlan {
                    name: String::new(),
                    columns: Vec::new(),
                    recursive: false,
                    materialization: crate::ast::CteMaterialization::Default,
                    search: None,
                    cycle: None,
                    body: CtePlanBody::Query(Box::new(empty_query())),
                })
            }
        })
    }
}
