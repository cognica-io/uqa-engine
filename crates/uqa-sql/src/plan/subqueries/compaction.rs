//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Admit a complete local remapping before changing query identities or dropping children.

use super::{command_arena, query_arena, subquery_id, visit_root, visit_source};
use crate::{
    plan::{CommandPlan, ExpressionPlan, QueryPlan, RelationalPlan},
    SQLError, ScalarExpr,
};
use uqa_core::memory::{ProductionControl, ProductionMap};

/// Compact one command's scalar-query arena in place. Failure before commit preserves the command; the commit neither allocates nor invokes fallible callbacks.
pub fn prune_command_with_control(
    command: &mut CommandPlan,
    control: ProductionControl<'_>,
) -> Result<(), SQLError> {
    prune_command_with_retention(command, control, |arena, keep| {
        let mut index = 0;
        arena.retain(|_| {
            let retained = keep(index);
            index += 1;
            retained
        });
    })
}

pub(crate) fn prune_command_with_retention(
    command: &mut CommandPlan,
    control: ProductionControl<'_>,
    retain: impl FnOnce(&mut Vec<QueryPlan>, &dyn Fn(usize) -> bool),
) -> Result<(), SQLError> {
    control.check()?;
    let Some(arena) = command_arena(command) else {
        return Ok(());
    };
    if arena.is_empty() {
        return Ok(());
    }
    let mut remap = Remapping::new(arena.len(), control);
    for expression in command.expressions() {
        remap.expression(expression)?;
    }
    if let Some(source) = command.source_input() {
        source.try_visit_expressions(&mut |expression| remap.expression(expression))?;
    }
    if remap.prepare()? {
        for expression in command.expressions_mut() {
            crate::plan::rewrite_scalar_expression(expression, &mut |node| remap.rewrite(node));
        }
        if let Some(source) = command.source_input_mut() {
            visit_source(source, &mut |node| remap.rewrite(node));
        }
        retain(
            command_arena(command).expect("command still owns its arena"),
            &|index| remap.slots.get(&index).is_some(),
        );
    }
    Ok(())
}

/// Compact only this query's arena, preserving child-query namespaces and canonical window inputs. Admission and cancellation finish before the allocation-free commit.
pub fn prune_query_with_control(
    query: &mut QueryPlan,
    control: ProductionControl<'_>,
) -> Result<(), SQLError> {
    prune_query_with_retention(query, control, |arena, keep| {
        let mut index = 0;
        arena.retain(|_| {
            let retained = keep(index);
            index += 1;
            retained
        });
    })
}

pub(crate) fn prune_query_with_retention(
    query: &mut QueryPlan,
    control: ProductionControl<'_>,
    retain: impl FnOnce(&mut Vec<QueryPlan>, &dyn Fn(usize) -> bool),
) -> Result<(), SQLError> {
    control.check()?;
    let arena = query_arena(&mut query.root);
    if arena.is_empty() {
        return Ok(());
    }
    let mut remap = Remapping::new(arena.len(), control);
    match &query.root {
        RelationalPlan::QueryBlock(block) => {
            block.try_visit_expressions(&mut |expression| remap.expression(expression))?;
        }
        RelationalPlan::Values { rows, .. } => {
            for expression in rows.iter().flatten() {
                remap.expression(expression)?;
            }
        }
        RelationalPlan::SetOp {
            order_by,
            limit,
            offset,
            ..
        } => {
            for expression in order_by
                .iter()
                .map(|order| &order.expr)
                .chain(limit.as_deref())
                .chain(offset.as_deref())
            {
                remap.expression(expression)?;
            }
        }
    }
    if remap.prepare()? {
        visit_root(&mut query.root, &mut |node| remap.rewrite(node));
        retain(query_arena(&mut query.root), &|index| {
            remap.slots.get(&index).is_some()
        });
    }
    Ok(())
}

/// Compact one scalar expression's arena without evaluating its expressions or cloning its retained children.
pub fn prune_expression_with_control(
    plan: &mut ExpressionPlan,
    control: ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    if plan.subqueries.is_empty() {
        return Ok(());
    }
    let mut remap = Remapping::new(plan.subqueries.len(), control);
    remap.expression(&plan.scalar)?;
    if remap.prepare()? {
        crate::plan::rewrite_scalar_expression(&mut plan.scalar, &mut |node| remap.rewrite(node));
        remap.retain(&mut plan.subqueries);
    }
    Ok(())
}

struct Remapping<'a> {
    slots: ProductionMap<'a, usize, usize>,
    arena_len: usize,
    invalid: bool,
    control: ProductionControl<'a>,
}

impl<'a> Remapping<'a> {
    fn new(arena_len: usize, control: ProductionControl<'a>) -> Self {
        Self {
            slots: ProductionMap::new(control),
            arena_len,
            invalid: false,
            control,
        }
    }

    fn expression(&mut self, expression: &ScalarExpr) -> Result<bool, SQLError> {
        expression.try_visit(&mut |node| {
            self.control.check()?;
            if let ScalarExpr::ScalarSubquery(id)
            | ScalarExpr::Exists { subquery: id, .. }
            | ScalarExpr::InSubquery { subquery: id, .. } = node
            {
                if *id >= self.arena_len {
                    self.invalid = true;
                } else if !self.invalid {
                    self.slots.insert_copy(*id, 0)?;
                }
            }
            Ok::<_, SQLError>(true)
        })?;
        Ok(true)
    }

    fn prepare(&mut self) -> Result<bool, SQLError> {
        self.control.check()?;
        // Invalid references retain their original diagnostic rather than aliasing a surviving child.
        if self.invalid || self.slots.len() == self.arena_len {
            return Ok(false);
        }
        let mut position = 0;
        for id in 0..self.arena_len {
            self.control.check()?;
            if self.slots.get(&id).is_some() {
                self.slots.insert_copy(id, position)?;
                position += 1;
            }
        }
        self.control.check()?;
        Ok(true)
    }

    fn rewrite(&self, expression: &mut ScalarExpr) {
        if let Some(id) = subquery_id(expression) {
            *id = *self.slots.get(id).expect("collected local subquery");
        }
    }

    fn retain(&self, arena: &mut Vec<QueryPlan>) {
        let mut id = 0;
        arena.retain(|_| {
            let keep = self.slots.get(&id).is_some();
            id += 1;
            keep
        });
    }
}

#[cfg(test)]
mod tests;
