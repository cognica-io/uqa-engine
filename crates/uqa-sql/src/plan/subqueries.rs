//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compact scalar-subquery arenas after expression rewrites discard inputs.

use crate::ScalarExpr;

use super::{CommandPlan, QueryPlan, RelationalPlan, SourcePlan};

mod compaction;
pub(in crate::plan) use compaction::{prepare_query_from, rewrite_query_from};
pub use compaction::{
    prune_command_with_control, prune_expression_with_control, prune_query_with_control,
};
pub(crate) use compaction::{
    prune_command_with_retention, prune_expression_with_retention, prune_query_with_retention,
};

mod transfer;
pub use transfer::transfer_expression;

mod transform;
pub(crate) use transform::{
    copy_occurrences, rewrite_expression_with_arena, rewrite_expression_with_arena_and_metadata,
    rewrite_query_with_arenas, rewrite_with_arenas, rewrite_with_arenas_and_metadata,
};

/// Drop unreferenced query children and remap local command expressions without evaluating them.
pub fn prune_command(command: &mut CommandPlan) {
    prune_command_with_control(command, uqa_core::memory::ProductionControl::uncontrolled())
        .expect("uncontrolled scalar-subquery compaction");
}

fn command_arena(command: &mut CommandPlan) -> Option<&mut Vec<QueryPlan>> {
    match command {
        CommandPlan::Insert(plan) => Some(&mut plan.subqueries),
        CommandPlan::Update(plan) => Some(&mut plan.subqueries),
        CommandPlan::Delete(plan) => Some(&mut plan.subqueries),
        CommandPlan::Merge(plan) => Some(&mut plan.subqueries),
        _ => None,
    }
}

/// Drop unreferenced query children and remap local query expressions, including canonical window definitions.
pub fn prune_query(query: &mut QueryPlan) {
    prune_query_with_control(query, uqa_core::memory::ProductionControl::uncontrolled())
        .expect("uncontrolled scalar-subquery compaction");
}

/// Retain only query children actually referenced by one scalar expression.
pub fn prune_expression(plan: &mut super::ExpressionPlan) {
    prune_expression_with_control(plan, uqa_core::memory::ProductionControl::uncontrolled())
        .expect("uncontrolled scalar-subquery compaction");
}

pub(in crate::plan) fn query_arena(root: &mut RelationalPlan) -> &mut Vec<QueryPlan> {
    match root {
        RelationalPlan::QueryBlock(block) => &mut block.subqueries,
        RelationalPlan::Values { subqueries, .. } | RelationalPlan::SetOp { subqueries, .. } => {
            subqueries
        }
    }
}

fn subquery_id(expression: &mut ScalarExpr) -> Option<&mut usize> {
    match expression {
        ScalarExpr::ScalarSubquery(id)
        | ScalarExpr::Exists { subquery: id, .. }
        | ScalarExpr::InSubquery { subquery: id, .. } => Some(id),
        _ => None,
    }
}

pub(crate) fn visit_root(root: &mut RelationalPlan, visitor: &mut dyn FnMut(&mut ScalarExpr)) {
    match root {
        RelationalPlan::QueryBlock(block) => visit_block(block, visitor),
        RelationalPlan::Values { rows, .. } => {
            for expression in rows.iter_mut().flatten() {
                super::rewrite_scalar_expression(expression, visitor);
            }
        }
        RelationalPlan::SetOp {
            order_by,
            limit,
            offset,
            ..
        } => {
            for expression in order_by
                .iter_mut()
                .map(|order| &mut order.expr)
                .chain(limit.as_deref_mut())
                .chain(offset.as_deref_mut())
            {
                super::rewrite_scalar_expression(expression, visitor);
            }
        }
    }
}

fn visit_source(source: &mut SourcePlan, visitor: &mut dyn FnMut(&mut ScalarExpr)) {
    let (children, expressions) = source.expression_inputs_mut();
    for child in children.into_iter().flatten() {
        visit_source(child, visitor);
    }
    for expression in expressions.flatten() {
        super::rewrite_scalar_expression(expression, visitor);
    }
}

fn visit_block(block: &mut super::QueryBlockPlan, visitor: &mut dyn FnMut(&mut ScalarExpr)) {
    if let Some(source) = &mut block.from {
        visit_source(source, visitor);
    }
    for expression in block
        .expression_steps_mut(super::query::ExpressionOrder::Rewrite)
        .flatten()
    {
        super::rewrite_scalar_expression(expression, visitor);
    }
}
