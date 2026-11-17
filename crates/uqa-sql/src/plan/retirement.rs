//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL plan nodes share scalar and child retirement across native construction and catalog input.

use super::{
    CommandPlan, CtePlan, CtePlanBody, ExpressionPlan, QueryPlan, RelationalPlan, SourcePlan,
};

mod commands;
pub(in crate::plan) mod construction;
pub(in crate::plan) mod layouts;
mod scalars;
mod tree;
pub(in crate::plan) mod windows;
pub(crate) use scalars::fields as scalar_fields;
pub(crate) use scalars::{Pending, Retire, ScalarRetirement};
pub(crate) use tree::{RetainedTree, RetirementFrame};
pub(in crate::plan) use windows::WindowRetirement;

#[cfg(test)]
pub(in crate::plan) mod tests;

#[derive(Debug)]
pub(crate) enum Node {
    Empty,
    Query(QueryPlan),
    Source(SourcePlan),
    Cte(CtePlan),
    Command(CommandPlan),
    Expression(ExpressionPlan),
}

impl Retire for Node {
    fn retire_step(
        &mut self,
        positions: &mut [usize],
        scalar: &mut Option<crate::ir::ownership::Retirement>,
    ) -> bool {
        match self {
            Node::Empty => true,
            Node::Query(query) => match &mut query.root {
                RelationalPlan::QueryBlock(block) => block.retire_step(positions, scalar),
                RelationalPlan::SetOp {
                    order_by,
                    limit,
                    offset,
                    ..
                } => scalars::fields!(positions, scalar; order_by, limit, offset),
                RelationalPlan::Values { rows, .. } => rows.retire_step(positions, scalar),
            },
            Node::Source(source) => source.retire_step(positions, scalar),
            Node::Cte(cte) => cte.retire_step(positions, scalar),
            Node::Command(command) => commands::retire_scalars(command, positions, scalar),
            Node::Expression(expression) => expression.scalar.retire_step(positions, scalar),
        }
    }
}

impl Node {
    pub(crate) fn retire_scalars(&mut self) {
        self.retire();
    }

    pub(crate) fn retire_child(&mut self, position: &mut usize) -> Option<Node> {
        match self {
            Node::Empty => None,
            Node::Query(query) => query_child(query, position),
            Node::Source(source) => source_child(source, position),
            Node::Command(command) => commands::child(command, position),
            Node::Expression(expression) => expression.subqueries.pop().map(Node::Query),
            Node::Cte(cte) => {
                if *position != 0 {
                    return None;
                }
                *position = 1;
                Some(match &mut cte.body {
                    CtePlanBody::Query(query) => {
                        Node::Query(std::mem::replace(query.as_mut(), empty_query()))
                    }
                    CtePlanBody::Command(command) => {
                        Node::Command(std::mem::replace(command.as_mut(), CommandPlan::Empty))
                    }
                })
            }
        }
    }
}

fn query_child(query: &mut QueryPlan, position: &mut usize) -> Option<Node> {
    if let Some(cte) = query.ctes.pop() {
        return Some(Node::Cte(cte));
    }
    match &mut query.root {
        RelationalPlan::QueryBlock(block) => block_child(block),
        RelationalPlan::Values { subqueries, .. } => subqueries.pop().map(Node::Query),
        RelationalPlan::SetOp {
            left,
            right,
            subqueries,
            ..
        } => {
            let child = match *position {
                0 => Some(Node::Query(std::mem::replace(left.as_mut(), empty_query()))),
                1 => Some(Node::Query(std::mem::replace(
                    right.as_mut(),
                    empty_query(),
                ))),
                _ => return subqueries.pop().map(Node::Query),
            };
            *position += 1;
            child
        }
    }
}

fn block_child(block: &mut super::QueryBlockPlan) -> Option<Node> {
    if let Some(source) = block.from.take() {
        return Some(Node::Source(source));
    }
    block.subqueries.pop().map(Node::Query)
}

fn source_child(source: &mut SourcePlan, position: &mut usize) -> Option<Node> {
    match source {
        SourcePlan::Table {
            executable_query, ..
        } => executable_query.take().map(|query| Node::Query(*query)),
        SourcePlan::Join { left, right, .. } => {
            let child = match *position {
                0 => Some(Node::Source(std::mem::replace(
                    left.as_mut(),
                    empty_source(),
                ))),
                1 => Some(Node::Source(std::mem::replace(
                    right.as_mut(),
                    empty_source(),
                ))),
                _ => return None,
            };
            *position += 1;
            child
        }
        SourcePlan::Subquery { body, .. } if *position == 0 => {
            *position = 1;
            Some(Node::Query(std::mem::replace(body.as_mut(), empty_query())))
        }
        SourcePlan::RetainedRows(_)
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. }
        | SourcePlan::Subquery { .. } => None,
    }
}

pub(crate) fn empty_query() -> QueryPlan {
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

pub(crate) fn empty_source() -> SourcePlan {
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
