//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Decode continuations retain enough capacity to retire their native forest without allocating.

use super::{empty_query, empty_source, retirement::Retire, Frame, NativeNode, Node, Owner};
use crate::plan::{CommandPlan, CtePlanBody, QueryPlan, RelationalPlan, SourcePlan};

mod commands;

impl<N: NativeNode> Drop for Owner<N> {
    fn drop(&mut self) {
        if !self.root.is_empty() {
            debug_assert!(self.frames.is_empty());
            let root = std::mem::replace(&mut self.root, N::empty());
            self.frames
                .push(Frame::new(root))
                .expect("admitted root retirement slot");
        }
        while let Some(mut frame) = self.frames.pop() {
            if !frame.retiring {
                frame.retiring = true;
                frame.position = 0;
                frame.count = None;
                N::retire_scalars(&mut frame);
            }
            if let Some(child) = N::retire_child(&mut frame) {
                debug_assert!(self.frames.capacity() >= self.frames.len() + 2);
                self.frames
                    .push(frame)
                    .expect("admitted parent retirement slot");
                self.frames
                    .push(Frame::new(child))
                    .expect("admitted child retirement slot");
            }
        }
    }
}

impl Frame {
    pub(super) fn retire_scalars(&mut self) {
        match &mut self.node {
            Node::Empty => {}
            Node::Query(query) => match &mut query.root {
                RelationalPlan::QueryBlock(block) => {
                    block.windows.retire();
                    block.projections.retire();
                    block.r#where.retire();
                    block.group_by.retire();
                    block.grouping_sets.retire();
                    block.having.retire();
                    block.order_by.retire();
                    block.limit.retire();
                    block.offset.retire();
                    block.distinct_on.retire();
                }
                RelationalPlan::SetOp {
                    order_by,
                    limit,
                    offset,
                    ..
                } => {
                    order_by.retire();
                    limit.retire();
                    offset.retire();
                }
                RelationalPlan::Values { rows, .. } => rows.retire(),
            },
            Node::Source(source) => match source {
                SourcePlan::Table { .. }
                | SourcePlan::Subquery { .. }
                | SourcePlan::RetainedRows(_) => {}
                SourcePlan::Join { on, .. } => on.retire(),
                SourcePlan::Values { rows, .. } => rows.retire(),
                SourcePlan::Function { args, .. } => args.retire(),
                SourcePlan::FunctionGroup { functions, .. } => {
                    for function in functions {
                        function.args.retire();
                    }
                }
            },
            Node::Cte(cte) => cte.cycle.retire(),
            Node::Command(command) => commands::retire_scalars(command),
            Node::Expression(expression) => expression.scalar.retire(),
        }
    }

    pub(super) fn retire_child(&mut self) -> Option<Node> {
        match &mut self.node {
            Node::Empty => None,
            Node::Query(query) => query_child(query, &mut self.position),
            Node::Source(source) => source_child(source, &mut self.position),
            Node::Command(command) => commands::child(command, &mut self.position),
            Node::Expression(expression) => expression.subqueries.pop().map(Node::Query),
            Node::Cte(cte) => {
                if self.position != 0 {
                    return None;
                }
                self.position = 1;
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
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = block.from.take() {
                return Some(Node::Source(source));
            }
            block.subqueries.pop().map(Node::Query)
        }
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
