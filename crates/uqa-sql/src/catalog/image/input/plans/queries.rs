//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::scalars::References;
use super::{driver::Step, empty_query, invalid, Fields, Input, Kind, Node, PlanMemory, SQLError};
use crate::{
    ast::SetOpKind,
    plan::{AccessPathPlan, ComputePlan, QueryBlockPlan, QueryPlan, RelationalPlan},
};
use uqa_core::memory::OwnedSet;

macro_rules! input_fields {
    ($input:ident, $read:ident $(, $async:ident)?) => {
        impl $input<'_> {
            pub(super) $($async)? fn plan_query_fields(
                &mut self,
                value: &mut QueryPlan,
                position: usize,
                count: &mut Option<usize>,
                references: &mut References,
                memory: &mut PlanMemory,
            ) -> Result<Step, SQLError> {
                match position {
                    0 => value.relations_bound = $read!(self.flag())?,
                    1 => {
                        return $read!(self.plan_children(
                            memory,
                            count,
                            &mut value.ctes,
                            Kind::Cte
                        ));
                    }
                    2 => value.root = $read!(self.plan_query_root(memory))?,
                    _ => {
                        return match &mut value.root {
                            RelationalPlan::QueryBlock(block) => {
                                $read!(self.plan_query_block_fields(
                                    block,
                                    position - 3,
                                    count,
                                    references,
                                    memory
                                ))
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
                                    0 => *kind = $read!(self.plan_set_kind())?,
                                    1 => *all = $read!(self.flag())?,
                                    2 | 3 => return Ok(Step::Child(Kind::Query)),
                                    4 => {
                                        *order_by =
                                            $read!(self.plan_orders(memory, references))?.take()
                                    }
                                    5 => {
                                        *limit = $read!(
                                            self.plan_optional_scalar_box(memory, references)
                                        )?
                                        .take()
                                    }
                                    6 => *with_ties = $read!(self.flag())?,
                                    7 => {
                                        *offset = $read!(
                                            self.plan_optional_scalar_box(memory, references)
                                        )?
                                        .take()
                                    }
                                    8 => {
                                        return $read!(self.plan_children(
                                            memory,
                                            count,
                                            subqueries,
                                            Kind::Query
                                        ));
                                    }
                                    _ => return Ok(Step::Done),
                                }
                                Ok(Step::Advance)
                            }
                            RelationalPlan::Values { rows, subqueries } => match position - 3 {
                                0 => {
                                    *rows = $read!(self.plan_rows(memory, references))?.take();
                                    Ok(Step::Advance)
                                }
                                1 => $read!(self.plan_children(
                                    memory,
                                    count,
                                    subqueries,
                                    Kind::Query
                                )),
                                _ => Ok(Step::Done),
                            },
                        };
                    }
                }
                Ok(Step::Advance)
            }
            $($async)? fn plan_query_root(
                &mut self,
                memory: &mut PlanMemory,
            ) -> Result<RelationalPlan, SQLError> {
                Ok(match $read!(self.binary.array::<1>())?[0] {
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
            $($async)? fn plan_query_block_fields(
                &mut self,
                value: &mut QueryBlockPlan,
                position: usize,
                count: &mut Option<usize>,
                references: &mut References,
                memory: &mut PlanMemory,
            ) -> Result<Step, SQLError> {
                match position {
                    0 => value.windows = $read!(self.plan_windows(memory, references))?.take(),
                    1 => {
                        value.projections =
                            $read!(self.plan_projections(memory, references))?.take()
                    }
                    2 => value.privilege_columns = $read!(self.plan_columns(memory))?,
                    3 => return $read!(self.plan_optional_child(Kind::Source)),
                    4 => {
                        value.r#where =
                            $read!(self.plan_optional_scalar(memory, references))?.take()
                    }
                    5 => value.compute = $read!(self.plan_compute())?,
                    6 => value.group_by = $read!(self.plan_scalars(memory, references))?.take(),
                    7 => value.grouping_sets = $read!(self.plan_rows(memory, references))?.take(),
                    8 => value.group_distinct = $read!(self.flag())?,
                    9 => {
                        value.having =
                            $read!(self.plan_optional_scalar(memory, references))?.take()
                    }
                    10 => value.order_by = $read!(self.plan_orders(memory, references))?.take(),
                    11 => {
                        value.limit = $read!(self.plan_optional_scalar(memory, references))?.take()
                    }
                    12 => value.with_ties = $read!(self.flag())?,
                    13 => {
                        value.offset =
                            $read!(self.plan_optional_scalar(memory, references))?.take()
                    }
                    14 => value.distinct = $read!(self.flag())?,
                    15 => {
                        value.distinct_on = $read!(self.plan_scalars(memory, references))?.take()
                    }
                    16 => {
                        return $read!(self.plan_children(
                            memory,
                            count,
                            &mut value.subqueries,
                            Kind::Query
                        ));
                    }
                    17 => value.access = $read!(self.plan_access())?,
                    18 => value.locking = $read!(self.plan_locking(memory))?,
                    _ => return Ok(Step::Done),
                }
                Ok(Step::Advance)
            }
        }
    };
}

input_fields!(Input, read_field);
input_fields!(Fields, await_field, async);

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
