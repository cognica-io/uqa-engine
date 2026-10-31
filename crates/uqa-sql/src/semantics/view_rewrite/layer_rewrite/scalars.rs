//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Local view-expression visitation keeps the original binding order and nested query boundaries.

use crate::{
    plan::{
        MutableQueryPlan, MutableSourcePlan, OwnedScalarExpr, QueryPlan, QueryScalarSlot,
        RelationalPlan, SourceChildSlot, SourcePlan, SourceScalarSlot,
    },
    SQLError,
};
use uqa_core::memory::ProductionControl;

pub(super) enum Order {
    Rename,
    Rewrite,
}
type Action<'a> = dyn FnMut(OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError> + 'a;

pub(super) fn query_count(query: &QueryPlan) -> usize {
    match &query.root {
        RelationalPlan::QueryBlock(block) => block.subqueries.len(),
        RelationalPlan::SetOp { subqueries, .. } | RelationalPlan::Values { subqueries, .. } => {
            subqueries.len()
        }
    }
}

pub(super) fn query(
    mut input: MutableQueryPlan,
    order: Order,
    control: &ProductionControl<'_>,
    action: &mut Action<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    control.check()?;
    match &input.root {
        RelationalPlan::QueryBlock(block) => {
            let (projections, groups, sets, orders, distinct) = (
                block.projections.len(),
                block.group_by.len(),
                block.grouping_sets.len(),
                block.order_by.len(),
                block.distinct_on.len(),
            );
            for index in 0..projections {
                input =
                    input.map_scalar(QueryScalarSlot::Projection(index), control, &mut *action)?;
            }
            input = input.map_scalar(QueryScalarSlot::Where, control, &mut *action)?;
            for index in 0..groups {
                input = input.map_scalar(QueryScalarSlot::GroupBy(index), control, &mut *action)?;
            }
            if matches!(order, Order::Rename) {
                for slot in [
                    QueryScalarSlot::Having,
                    QueryScalarSlot::Limit,
                    QueryScalarSlot::Offset,
                ] {
                    input = input.map_scalar(slot, control, &mut *action)?;
                }
                for index in 0..distinct {
                    input = input.map_scalar(
                        QueryScalarSlot::DistinctOn(index),
                        control,
                        &mut *action,
                    )?;
                }
            }
            for set in 0..sets {
                let RelationalPlan::QueryBlock(block) = &input.root else {
                    unreachable!()
                };
                for expression in 0..block.grouping_sets[set].len() {
                    input = input.map_scalar(
                        QueryScalarSlot::GroupingSet { set, expression },
                        control,
                        &mut *action,
                    )?;
                }
            }
            if matches!(order, Order::Rewrite) {
                input = input.map_scalar(QueryScalarSlot::Having, control, &mut *action)?;
            }
            for index in 0..orders {
                input = input.map_scalar(QueryScalarSlot::OrderBy(index), control, &mut *action)?;
            }
            if matches!(order, Order::Rewrite) {
                for slot in [QueryScalarSlot::Limit, QueryScalarSlot::Offset] {
                    input = input.map_scalar(slot, control, &mut *action)?;
                }
                for index in 0..distinct {
                    input = input.map_scalar(
                        QueryScalarSlot::DistinctOn(index),
                        control,
                        &mut *action,
                    )?;
                }
            }
        }
        RelationalPlan::SetOp { order_by, .. } => {
            for index in 0..order_by.len() {
                input = input.map_scalar(QueryScalarSlot::OrderBy(index), control, &mut *action)?;
            }
            for slot in [QueryScalarSlot::Limit, QueryScalarSlot::Offset] {
                input = input.map_scalar(slot, control, &mut *action)?;
            }
        }
        RelationalPlan::Values { rows, .. } => {
            for row in 0..rows.len() {
                let RelationalPlan::Values { rows, .. } = &input.root else {
                    unreachable!()
                };
                for column in 0..rows[row].len() {
                    input = input.map_scalar(
                        QueryScalarSlot::Value { row, column },
                        control,
                        &mut *action,
                    )?;
                }
            }
        }
    }
    Ok(input)
}

/// Source scalar roots exclude derived queries, which bind their own namespace.
pub(super) fn source(
    mut input: MutableSourcePlan,
    control: &ProductionControl<'_>,
    action: &mut Action<'_>,
) -> Result<MutableSourcePlan, SQLError> {
    control.check()?;
    match &*input {
        SourcePlan::Join { .. } => {
            for slot in [SourceChildSlot::Left, SourceChildSlot::Right] {
                input = input.map_child(slot, control, |input| source(input, control, action))?;
            }
            input = input.map_scalar(SourceScalarSlot::JoinOn, control, action)?;
        }
        SourcePlan::Values { rows, .. } => {
            for row in 0..rows.len() {
                let SourcePlan::Values { rows, .. } = &*input else {
                    unreachable!()
                };
                for column in 0..rows[row].len() {
                    input = input.map_scalar(
                        SourceScalarSlot::Value { row, column },
                        control,
                        &mut *action,
                    )?;
                }
            }
        }
        SourcePlan::Function { args, .. } => {
            for index in 0..args.len() {
                input =
                    input.map_scalar(SourceScalarSlot::Argument(index), control, &mut *action)?;
            }
        }
        SourcePlan::FunctionGroup { functions, .. } => {
            for function in 0..functions.len() {
                let SourcePlan::FunctionGroup { functions, .. } = &*input else {
                    unreachable!()
                };
                for argument in 0..functions[function].args.len() {
                    input = input.map_scalar(
                        SourceScalarSlot::GroupArgument { function, argument },
                        control,
                        &mut *action,
                    )?;
                }
            }
        }
        _ => {}
    }
    Ok(input)
}
