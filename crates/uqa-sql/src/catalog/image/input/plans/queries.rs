//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::scalars::References;
use super::{driver::Step, empty_query, invalid, Input, Kind, Node, PlanMemory, SQLError};
use crate::{
    ast::SetOpKind,
    plan::{AccessPathPlan, ComputePlan, QueryBlockPlan, QueryPlan, RelationalPlan},
};
use uqa_core::memory::OwnedSet;

pub(super) fn read(
    value: &mut QueryPlan,
    position: usize,
    count: &mut Option<usize>,
    references: &mut References,
    input: &mut Input<'_>,
    memory: &mut PlanMemory,
) -> Result<Step, SQLError> {
    match position {
        0 => value.relations_bound = input.flag()?,
        1 => return input.plan_children(memory, count, &mut value.ctes, Kind::Cte),
        2 => value.root = root(input, memory)?,
        _ => {
            return match &mut value.root {
                RelationalPlan::QueryBlock(block) => {
                    block_fields(block, position - 3, count, references, input, memory)
                }
                RelationalPlan::SetOp {
                    kind,
                    all,
                    left: _,
                    right: _,
                    order_by,
                    limit,
                    with_ties,
                    offset,
                    subqueries,
                } => {
                    match position - 3 {
                        0 => *kind = input.plan_set_kind()?,
                        1 => *all = input.flag()?,
                        2 | 3 => return Ok(Step::Child(Kind::Query)),
                        4 => *order_by = input.plan_orders(memory, references)?.take(),
                        5 => *limit = input.plan_optional_scalar_box(memory, references)?.take(),
                        6 => *with_ties = input.flag()?,
                        7 => *offset = input.plan_optional_scalar_box(memory, references)?.take(),
                        8 => return input.plan_children(memory, count, subqueries, Kind::Query),
                        _ => return Ok(Step::Done),
                    }
                    Ok(Step::Advance)
                }
                RelationalPlan::Values { rows, subqueries } => match position - 3 {
                    0 => {
                        *rows = input.plan_rows(memory, references)?.take();
                        Ok(Step::Advance)
                    }
                    1 => input.plan_children(memory, count, subqueries, Kind::Query),
                    _ => Ok(Step::Done),
                },
            }
        }
    }
    Ok(Step::Advance)
}

fn root(input: &mut Input<'_>, memory: &mut PlanMemory) -> Result<RelationalPlan, SQLError> {
    Ok(match input.binary.array::<1>()?[0] {
        0 => {
            memory.grow(size_of::<QueryBlockPlan>())?;
            RelationalPlan::QueryBlock(Box::new(QueryBlockPlan {
                deferred_outer_rewrites: Vec::new(),
                windows: Vec::new(),
                projections: Vec::new(),
                privilege_columns: OwnedSet::new(),
                from: None,
                r#where: None,
                compute: ComputePlan::Project,
                group_by: Vec::new(),
                grouping_sets: Vec::new(),
                group_distinct: false,
                having: None,
                order_by: Vec::new(),
                limit: None,
                with_ties: false,
                offset: None,
                distinct: false,
                distinct_on: Vec::new(),
                subqueries: Vec::new(),
                access: AccessPathPlan::Row,
                locking: Vec::new(),
            }))
        }
        1 => {
            memory.grow(2 * size_of::<QueryPlan>())?;
            RelationalPlan::SetOp {
                kind: SetOpKind::Union,
                all: false,
                left: Box::new(empty_query()),
                right: Box::new(empty_query()),
                order_by: Vec::new(),
                limit: None,
                with_ties: false,
                offset: None,
                subqueries: Vec::new(),
            }
        }
        2 => RelationalPlan::Values {
            rows: Vec::new(),
            subqueries: Vec::new(),
        },
        _ => return Err(invalid("relational query constructor")),
    })
}

fn block_fields(
    value: &mut QueryBlockPlan,
    position: usize,
    count: &mut Option<usize>,
    references: &mut References,
    input: &mut Input<'_>,
    memory: &mut PlanMemory,
) -> Result<Step, SQLError> {
    match position {
        0 => value.windows = input.plan_windows(memory, references)?.take(),
        1 => value.projections = input.plan_projections(memory, references)?.take(),
        2 => value.privilege_columns = input.plan_columns(memory)?,
        3 => return input.plan_optional_child(Kind::Source),
        4 => value.r#where = input.plan_optional_scalar(memory, references)?.take(),
        5 => value.compute = input.plan_compute()?,
        6 => value.group_by = input.plan_scalars(memory, references)?.take(),
        7 => value.grouping_sets = input.plan_rows(memory, references)?.take(),
        8 => value.group_distinct = input.flag()?,
        9 => value.having = input.plan_optional_scalar(memory, references)?.take(),
        10 => value.order_by = input.plan_orders(memory, references)?.take(),
        11 => value.limit = input.plan_optional_scalar(memory, references)?.take(),
        12 => value.with_ties = input.flag()?,
        13 => value.offset = input.plan_optional_scalar(memory, references)?.take(),
        14 => value.distinct = input.flag()?,
        15 => value.distinct_on = input.plan_scalars(memory, references)?.take(),
        16 => return input.plan_children(memory, count, &mut value.subqueries, Kind::Query),
        17 => value.access = input.plan_access()?,
        18 => value.locking = input.plan_locking(memory)?,
        _ => return Ok(Step::Done),
    }
    Ok(Step::Advance)
}

pub(super) fn attach(value: &mut QueryPlan, position: &mut usize, child: Node) {
    if let Node::Cte(cte) = child {
        value.ctes.push(cte);
        return;
    }
    match (&mut value.root, child) {
        (RelationalPlan::QueryBlock(block), Node::Source(source)) => {
            block.from = Some(source);
            *position += 1;
        }
        (RelationalPlan::QueryBlock(block), Node::Query(query)) => block.subqueries.push(query),
        (RelationalPlan::Values { subqueries, .. }, Node::Query(query)) => subqueries.push(query),
        (
            RelationalPlan::SetOp {
                left,
                right,
                subqueries,
                ..
            },
            Node::Query(query),
        ) => match *position {
            5 => {
                **left = query;
                *position += 1;
            }
            6 => {
                **right = query;
                *position += 1;
            }
            11 => subqueries.push(query),
            _ => unreachable!("set-operation query slot"),
        },
        _ => unreachable!("query child constructor"),
    }
}
