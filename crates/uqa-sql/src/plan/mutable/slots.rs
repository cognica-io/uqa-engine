//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed scalar locations share the query constructor's field ownership.

use super::{memory::RewriteMemory, QueryPlan, RelationalPlan, ScalarExpr};

/// A scalar root in one query's own expression namespace. Nested sources and query children retain separate owners.
#[derive(Clone, Copy, Debug)]
pub enum QueryScalarSlot {
    Projection(usize),
    Where,
    GroupBy(usize),
    GroupingSet { set: usize, expression: usize },
    Having,
    OrderBy(usize),
    Limit,
    Offset,
    DistinctOn(usize),
    WindowPartition { window: usize, expression: usize },
    WindowOrder { window: usize, expression: usize },
    WindowFrameStart(usize),
    WindowFrameEnd(usize),
    Value { row: usize, column: usize },
}

impl QueryScalarSlot {
    pub(super) fn expression_mut(self, query: &mut QueryPlan) -> Option<&mut ScalarExpr> {
        match &mut query.root {
            RelationalPlan::QueryBlock(block) => match self {
                Self::Projection(index) => Some(&mut block.projections[index].expr),
                Self::Where => block.r#where.as_mut(),
                Self::GroupBy(index) => Some(&mut block.group_by[index]),
                Self::GroupingSet { set, expression } => {
                    Some(&mut block.grouping_sets[set][expression])
                }
                Self::Having => block.having.as_mut(),
                Self::OrderBy(index) => Some(&mut block.order_by[index].expr),
                Self::Limit => block.limit.as_mut(),
                Self::Offset => block.offset.as_mut(),
                Self::DistinctOn(index) => Some(&mut block.distinct_on[index]),
                Self::WindowPartition { window, expression } => {
                    Some(&mut block.windows[window].spec.partition_by[expression])
                }
                Self::WindowOrder { window, expression } => {
                    Some(&mut block.windows[window].spec.order_by[expression].expr)
                }
                Self::WindowFrameStart(window) | Self::WindowFrameEnd(window) => {
                    let frame = block.windows[window].spec.frame.as_mut()?;
                    match if matches!(self, Self::WindowFrameStart(_)) {
                        &mut frame.start
                    } else {
                        &mut frame.end
                    } {
                        crate::ScalarFrameBound::Preceding(expression)
                        | crate::ScalarFrameBound::Following(expression) => Some(expression),
                        _ => None,
                    }
                }
                Self::Value { .. } => panic!("VALUES scalar belongs to a VALUES root"),
            },
            RelationalPlan::Values { rows, .. } => match self {
                Self::Value { row, column } => Some(&mut rows[row][column]),
                _ => panic!("query-block scalar belongs to a query block"),
            },
            RelationalPlan::SetOp {
                order_by,
                limit,
                offset,
                ..
            } => match self {
                Self::OrderBy(index) => Some(&mut order_by[index].expr),
                Self::Limit => limit.as_deref_mut(),
                Self::Offset => offset.as_deref_mut(),
                _ => panic!("set-operation scalar belongs to its ordering or slice"),
            },
        }
    }

    pub(super) fn memory_mut(self, memory: &mut RewriteMemory) -> &mut Option<RewriteMemory> {
        let root = memory.field_mut("root").as_mut().expect("query root");
        let has_block = match root {
            RewriteMemory::Fields { fields, .. } => {
                fields.iter().any(|field| field.name == "block")
            }
            RewriteMemory::Scalar(_) => unreachable!("query root owner"),
        };
        let root = if has_block {
            root.field_mut("block").as_mut().expect("query block")
        } else {
            root
        };
        match self {
            Self::Projection(index) => item(root, "projections", index).scalar_mut(),
            Self::Where => field(root, "where").scalar_mut(),
            Self::GroupBy(index) => item(root, "group_by", index).scalar_mut(),
            Self::GroupingSet { set, expression } => item(root, "grouping_sets", set)
                .item_mut(expression)
                .as_mut()
                .expect("grouping-set expression")
                .scalar_mut(),
            Self::Having => field(root, "having").scalar_mut(),
            Self::OrderBy(index) => item(root, "order_by", index).scalar_mut(),
            Self::Limit => field(root, "limit").scalar_mut(),
            Self::Offset => field(root, "offset").scalar_mut(),
            Self::DistinctOn(index) => item(root, "distinct_on", index).scalar_mut(),
            Self::WindowPartition { window, expression } => item(
                field(item(root, "windows", window), "spec"),
                "partition_by",
                expression,
            )
            .scalar_mut(),
            Self::WindowOrder { window, expression } => item(
                field(item(root, "windows", window), "spec"),
                "order_by",
                expression,
            )
            .scalar_mut(),
            Self::WindowFrameStart(window) | Self::WindowFrameEnd(window) => field(
                field(field(item(root, "windows", window), "spec"), "frame"),
                if matches!(self, Self::WindowFrameStart(_)) {
                    "start"
                } else {
                    "end"
                },
            )
            .scalar_mut(),
            Self::Value { row, column } => item(root, "rows", row)
                .item_mut(column)
                .as_mut()
                .expect("VALUES cell")
                .scalar_mut(),
        }
    }
}

fn field<'a>(memory: &'a mut RewriteMemory, name: &str) -> &'a mut RewriteMemory {
    memory.field_mut(name).as_mut().expect("copied query field")
}

fn item<'a>(memory: &'a mut RewriteMemory, name: &str, index: usize) -> &'a mut RewriteMemory {
    field(memory, name)
        .item_mut(index)
        .as_mut()
        .expect("copied query item")
}
