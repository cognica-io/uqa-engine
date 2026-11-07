//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native plan reconstruction keeps every partial node inside one admitted owner.

mod api;
mod assignments;
mod ctes;
mod driver;
mod fields;
mod forest;
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
pub use unified::CapturedPlan;

use super::{invalid, Input};
use crate::{
    plan::{
        CommandPlan, CtePlan, CtePlanBody, ExpressionPlan, QueryPlan, RelationalPlan, SourcePlan,
    },
    SQLError, ScalarExpr,
};
use uqa_core::memory::{BudgetedVec, MemoryBudget};

#[derive(Debug)]
pub(super) enum Node {
    Empty,
    Query(QueryPlan),
    Source(SourcePlan),
    Cte(CtePlan),
    Command(CommandPlan),
    Expression(ExpressionPlan),
}

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
    root: N,
    frames: BudgetedVec<Frame<N>>,
    memory: PlanMemory,
}

impl<N: NativeNode> Owner<N> {
    fn new(memory: &MemoryBudget) -> Self {
        Self {
            root: N::empty(),
            frames: BudgetedVec::new(memory),
            memory: PlanMemory::new(memory),
        }
    }

    fn begin(&mut self, kind: N::Kind) -> Result<(), SQLError> {
        // One extra slot covers a required, still-empty boxed child during partial reconstruction. Retirement reuses this capacity and never acquires another allowance.
        self.frames.reserve(2)?;
        let node = N::new(kind, &mut self.memory)?;
        self.frames.push(Frame::new(node))?;
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

fn empty_query() -> QueryPlan {
    QueryPlan {
        analyzed_output: None,
        analyzed_source: None,
        analyzed_grouping: None,
        relations_bound: false,
        relation_privilege_subject: None,
        ctes: Vec::new(),
        root: RelationalPlan::Values {
            rows: Vec::new(),
            subqueries: Vec::new(),
        },
    }
}

fn empty_source() -> SourcePlan {
    SourcePlan::Values {
        lateral: false,
        rows: Vec::new(),
        alias: None,
        column_aliases: Vec::new(),
        internal_relation: None,
        internal_column_types: Vec::new(),
        internal_column_collations: Vec::new(),
    }
}
