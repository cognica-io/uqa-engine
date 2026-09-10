//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compact scalar-subquery arenas after expression rewrites discard inputs.

use std::collections::BTreeMap;
use uqa_execution::ScalarExpr;

use crate::{CommandPlan, QueryPlan, RelationalPlan, SourcePlan};

pub(super) fn prune_command(command: &mut CommandPlan) {
    let Some(arena) = command_arena(command) else {
        return;
    };
    let arena = std::mem::take(arena);
    let arena = compact(arena, |visitor| {
        for expression in command.expressions_mut() {
            crate::rewrite_scalar_expression(expression, visitor);
        }
        if let Some(source) = command.source_input_mut() {
            visit_source(source, visitor);
        }
    });
    if let Some(target) = command_arena(command) {
        *target = arena;
    }
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

pub(super) fn prune_query(query: &mut QueryPlan) {
    let arena = std::mem::take(query_arena(&mut query.root));
    let arena = compact(arena, |visitor| visit_root(&mut query.root, visitor));
    *query_arena(&mut query.root) = arena;
}

fn query_arena(root: &mut RelationalPlan) -> &mut Vec<QueryPlan> {
    match root {
        RelationalPlan::QueryBlock(block) => &mut block.subqueries,
        RelationalPlan::Values { subqueries, .. } | RelationalPlan::SetOp { subqueries, .. } => {
            subqueries
        }
    }
}

fn compact(
    arena: Vec<QueryPlan>,
    mut visit: impl FnMut(&mut dyn FnMut(&mut ScalarExpr)),
) -> Vec<QueryPlan> {
    if arena.is_empty() {
        return arena;
    }
    let mut remap = BTreeMap::new();
    visit(&mut |expression| {
        if let Some(id) = subquery_id(expression) {
            remap.insert(*id, 0);
        }
    });
    // Invalid references retain their original diagnostic rather than aliasing
    // a surviving entry after compaction.
    if remap.keys().any(|id| *id >= arena.len()) || remap.len() == arena.len() {
        return arena;
    }
    for (position, value) in remap.values_mut().enumerate() {
        *value = position;
    }
    visit(&mut |expression| {
        if let Some(id) = subquery_id(expression) {
            *id = remap[id];
        }
    });
    arena
        .into_iter()
        .enumerate()
        .filter_map(|(id, query)| remap.contains_key(&id).then_some(query))
        .collect()
}

fn subquery_id(expression: &mut ScalarExpr) -> Option<&mut usize> {
    match expression {
        ScalarExpr::ScalarSubquery(id)
        | ScalarExpr::Exists { subquery: id, .. }
        | ScalarExpr::InSubquery { subquery: id, .. } => Some(id),
        _ => None,
    }
}

fn visit_root(root: &mut RelationalPlan, visitor: &mut dyn FnMut(&mut ScalarExpr)) {
    match root {
        RelationalPlan::QueryBlock(block) => {
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
            {
                crate::rewrite_scalar_expression(expression, visitor);
            }
        }
        RelationalPlan::Values { rows, .. } => {
            for expression in rows.iter_mut().flatten() {
                crate::rewrite_scalar_expression(expression, visitor);
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
                crate::rewrite_scalar_expression(expression, visitor);
            }
        }
    }
}

fn visit_source(source: &mut SourcePlan, visitor: &mut dyn FnMut(&mut ScalarExpr)) {
    match source {
        SourcePlan::Join {
            left, right, on, ..
        } => {
            visit_source(left, visitor);
            visit_source(right, visitor);
            if let Some(on) = on {
                crate::rewrite_scalar_expression(on, visitor);
            }
        }
        SourcePlan::Values { rows, .. } => {
            for expression in rows.iter_mut().flatten() {
                crate::rewrite_scalar_expression(expression, visitor);
            }
        }
        SourcePlan::Function { args, .. } => {
            for argument in args {
                crate::rewrite_scalar_expression(argument, visitor);
            }
        }
        SourcePlan::FunctionGroup { functions, .. } => {
            for argument in functions.iter_mut().flat_map(|function| &mut function.args) {
                crate::rewrite_scalar_expression(argument, visitor);
            }
        }
        // A derived query owns its own scalar-subquery arena.
        SourcePlan::Subquery { .. } | SourcePlan::Table { .. } => {}
    }
}
