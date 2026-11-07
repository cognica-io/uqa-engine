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
mod metadata;
mod mutations;
mod ownership;
mod queries;
mod retirement;
mod sources;
#[cfg(test)]
mod tests;
mod validation;
mod windows;

pub use api::{CapturedExpressionPlan, CapturedQuery};

use super::{invalid, Input};
use crate::{
    plan::{
        CommandPlan, CtePlan, CtePlanBody, ExpressionPlan, QueryPlan, RelationalPlan, SourcePlan,
    },
    SQLError, ScalarExpr,
};
use uqa_core::memory::{BudgetedVec, MemoryBudget, MemoryReservation};

#[derive(Debug)]
enum Node {
    Empty,
    Query(QueryPlan),
    Source(SourcePlan),
    Cte(CtePlan),
    Command(CommandPlan),
    Expression(ExpressionPlan),
}

#[derive(Clone, Copy)]
enum Kind {
    Query,
    Source,
    Cte,
    Command,
    Expression,
}

#[derive(Debug)]
struct Frame {
    node: Node,
    position: usize,
    count: Option<usize>,
    retiring: bool,
    references: super::scalars::References,
    rule_references: super::scalars::References,
}

impl Frame {
    fn new(node: Node) -> Self {
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
struct Owner {
    root: Node,
    frames: BudgetedVec<Frame>,
    memory: MemoryReservation,
}

impl Owner {
    fn new(memory: &MemoryBudget) -> Self {
        Self {
            root: Node::Empty,
            frames: BudgetedVec::new(memory),
            memory: memory.empty_reservation(),
        }
    }

    fn begin(&mut self, kind: Kind) -> Result<(), SQLError> {
        // One extra slot covers a required, still-empty boxed child during partial reconstruction. Retirement reuses this capacity and never acquires another allowance.
        self.frames.reserve(2)?;
        let node = match kind {
            Kind::Query => Node::Query(empty_query()),
            Kind::Source => Node::Source(empty_source()),
            Kind::Command => Node::Command(CommandPlan::Empty),
            Kind::Expression => Node::Expression(ExpressionPlan {
                scalar: ScalarExpr::Star,
                subqueries: Vec::new(),
            }),
            Kind::Cte => {
                self.memory.grow(size_of::<QueryPlan>())?;
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
        };
        self.frames.push(Frame::new(node))?;
        Ok(())
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
