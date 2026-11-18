//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Admit a complete local remapping before changing query identities or dropping children.

use super::{command_arena, query_arena, subquery_id, visit_root, visit_source};
use crate::{
    catalog::work::{self, Work},
    ir::{
        children::Scope,
        observation::{self, Decision},
    },
    plan::{CommandPlan, ExpressionPlan, QueryPlan, RelationalPlan},
    SQLError, ScalarExpr,
};
use uqa_core::memory::{ProductionControl, ProductionMap};

mod input;
pub(in crate::plan) use input::rewrite as rewrite_query_from;

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
    let Some(remap) = work::inline(prepare_query_from(query, control, &mut ()))? else {
        return Ok(());
    };
    visit_root(&mut query.root, &mut |node| remap.rewrite(node));
    retain(query_arena(&mut query.root), &|index| remap.keep(index));
    Ok(())
}

pub(in crate::plan) async fn prepare_query_from<'a, W: Work + Send>(
    query: &QueryPlan,
    control: ProductionControl<'a>,
    work: &mut W,
) -> Result<Option<Remapping<'a>>, SQLError> {
    control.check()?;
    let count = match &query.root {
        RelationalPlan::QueryBlock(block) => block.subqueries.len(),
        RelationalPlan::Values { subqueries, .. } | RelationalPlan::SetOp { subqueries, .. } => {
            subqueries.len()
        }
    };
    if count == 0 {
        return Ok(None);
    }
    let mut remap = Remapping::new(count, control);
    input::collect(query, &mut remap, &control, work).await?;
    if remap.prepare_from(work).await? {
        Ok(Some(remap))
    } else {
        remap.retire_from(work).await?;
        Ok(None)
    }
}

/// Compact one scalar expression's arena without evaluating its expressions or cloning its retained children.
pub fn prune_expression_with_control(
    plan: &mut ExpressionPlan,
    control: ProductionControl<'_>,
) -> Result<(), SQLError> {
    prune_expression_with_retention(plan, control, |arena, keep| {
        let mut index = 0;
        arena.retain(|_| {
            let retained = keep(index);
            index += 1;
            retained
        });
    })
}

pub(crate) fn prune_expression_with_retention(
    plan: &mut ExpressionPlan,
    control: ProductionControl<'_>,
    retain: impl FnOnce(&mut Vec<QueryPlan>, &dyn Fn(usize) -> bool),
) -> Result<(), SQLError> {
    control.check()?;
    if plan.subqueries.is_empty() {
        return Ok(());
    }
    let mut remap = Remapping::new(plan.subqueries.len(), control);
    remap.expression(&plan.scalar)?;
    if remap.prepare()? {
        crate::plan::rewrite_scalar_expression(&mut plan.scalar, &mut |node| remap.rewrite(node));
        retain(&mut plan.subqueries, &|index| {
            remap.slots.get(&index).is_some()
        });
    }
    Ok(())
}

pub(in crate::plan) struct Remapping<'a> {
    slots: ProductionMap<'a, usize, usize>,
    arena_len: usize,
    invalid: bool,
    control: ProductionControl<'a>,
}

impl<'a> Remapping<'a> {
    pub(in crate::plan) async fn retire_from<W: Work>(
        &mut self,
        work: &mut W,
    ) -> Result<(), SQLError> {
        for index in 0..self.arena_len {
            if self.slots.is_empty() {
                break;
            }
            self.control.check()?;
            work.checkpoint().await?;
            self.control.check()?;
            self.slots.remove(&index);
        }
        Ok(())
    }

    pub(in crate::plan) fn keep(&self, index: usize) -> bool {
        self.slots.get(&index).is_some()
    }
    fn new(arena_len: usize, control: ProductionControl<'a>) -> Self {
        Self {
            slots: ProductionMap::new(control),
            arena_len,
            invalid: false,
            control,
        }
    }

    fn expression(&mut self, expression: &ScalarExpr) -> Result<bool, SQLError> {
        work::inline(self.expression_from(expression, &mut ()))?;
        Ok(true)
    }

    async fn expression_from<W: Work>(
        &mut self,
        expression: &ScalarExpr,
        work: &mut W,
    ) -> Result<(), SQLError> {
        let control = self.control;
        observation::visit_in(expression, &control, work, Scope::Complete, &mut |node| {
            self.observe(node)?;
            Ok(Decision::Descend)
        })
        .await?;
        Ok(())
    }

    fn observe(&mut self, node: &ScalarExpr) -> Result<(), SQLError> {
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
        Ok(())
    }

    fn prepare(&mut self) -> Result<bool, SQLError> {
        work::inline(self.prepare_from(&mut ()))
    }

    async fn prepare_from<W: Work>(&mut self, work: &mut W) -> Result<bool, SQLError> {
        self.control.check()?;
        // Invalid references retain their original diagnostic rather than aliasing a surviving child.
        if self.invalid || self.slots.len() == self.arena_len {
            return Ok(false);
        }
        let mut position = 0;
        for id in 0..self.arena_len {
            self.control.check()?;
            work.checkpoint().await?;
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
}

#[cfg(test)]
mod tests;
