//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compact scalar-subquery arenas after expression rewrites discard inputs.

use crate::ScalarExpr;

use super::{CommandPlan, QueryPlan, RelationalPlan, SourcePlan};

mod compaction;
pub use compaction::{
    prune_command_with_control, prune_expression_with_control, prune_query_with_control,
};

mod transfer;
pub use transfer::transfer_expression;

mod transform;
pub(crate) use transform::{
    copy_occurrences, query_nodes as rewrite_query_with_arenas, rewrite_expression_with_arena,
    rewrite_with_arenas,
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

fn query_arena(root: &mut RelationalPlan) -> &mut Vec<QueryPlan> {
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
    match source {
        crate::plan::SourcePlan::RetainedRows(_) => {}
        SourcePlan::Join {
            left, right, on, ..
        } => {
            visit_source(left, visitor);
            visit_source(right, visitor);
            if let Some(on) = on {
                super::rewrite_scalar_expression(on, visitor);
            }
        }
        SourcePlan::Values { rows, .. } => {
            for expression in rows.iter_mut().flatten() {
                super::rewrite_scalar_expression(expression, visitor);
            }
        }
        SourcePlan::Function { args, .. } => {
            for argument in args {
                super::rewrite_scalar_expression(argument, visitor);
            }
        }
        SourcePlan::FunctionGroup { functions, .. } => {
            for argument in functions.iter_mut().flat_map(|function| &mut function.args) {
                super::rewrite_scalar_expression(argument, visitor);
            }
        }
        // A derived query owns its own scalar-subquery arena.
        SourcePlan::Subquery { .. } | SourcePlan::Table { .. } => {}
    }
}

fn visit_block(block: &mut super::QueryBlockPlan, visitor: &mut dyn FnMut(&mut ScalarExpr)) {
    if let Some(source) = &mut block.from {
        visit_source(source, visitor);
    }
    for expression in block
        .projections
        .iter_mut()
        .map(|projection| &mut projection.expr)
        .chain(block.r#where.iter_mut())
        .chain(block.group_by.iter_mut())
        .chain(block.grouping_sets.iter_mut().flatten())
        .chain(block.having.iter_mut())
        .chain(block.order_by.iter_mut().map(|order| &mut order.expr))
        .chain(block.limit.iter_mut())
        .chain(block.offset.iter_mut())
        .chain(block.distinct_on.iter_mut())
        .chain(
            block
                .windows
                .iter_mut()
                .flat_map(|window| window.spec.expressions_mut()),
        )
    {
        super::rewrite_scalar_expression(expression, visitor);
    }
}
