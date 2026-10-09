//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rewrite scalar nodes with their owning query arena. Nested arenas are completed first, so copied queries already retain their own analyzed inputs.

use super::{command_arena, query_arena, subquery_id, visit_root, visit_source};
use crate::{
    plan::{CommandPlan, CtePlan, CtePlanBody, QueryPlan, RelationalPlan, SourcePlan, UnifiedPlan},
    SQLError, ScalarExpr,
};

type Rewrite<'a> = &'a mut dyn FnMut(&mut ScalarExpr, &mut Vec<QueryPlan>) -> Result<(), SQLError>;

pub(crate) fn rewrite_with_arenas(
    plan: &mut UnifiedPlan,
    rewrite: Rewrite<'_>,
) -> Result<(), SQLError> {
    match plan {
        UnifiedPlan::Query(query) => query_nodes(query, rewrite),
        UnifiedPlan::Command(command) => command_nodes(command, rewrite),
    }
}

pub(crate) fn rewrite_expression_with_arena(
    expression: &mut ScalarExpr,
    arena: &mut Vec<QueryPlan>,
    rewrite: Rewrite<'_>,
) -> Result<(), SQLError> {
    for query in arena.iter_mut() {
        query_nodes(query, rewrite)?;
    }
    let mut result = Ok(());
    crate::plan::rewrite_scalar_expression(expression, &mut |node| {
        if result.is_ok() {
            result = rewrite(node, arena);
        }
    });
    result
}

pub(crate) fn query_nodes(query: &mut QueryPlan, rewrite: Rewrite<'_>) -> Result<(), SQLError> {
    cte_nodes(&mut query.ctes, rewrite)?;
    match &mut query.root {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = &mut block.from {
                source_nodes(source, rewrite)?;
            }
        }
        RelationalPlan::SetOp { left, right, .. } => {
            query_nodes(left, rewrite)?;
            query_nodes(right, rewrite)?;
        }
        RelationalPlan::Values { .. } => {}
    }
    for child in query_arena(&mut query.root).iter_mut() {
        query_nodes(child, rewrite)?;
    }
    let mut arena = std::mem::take(query_arena(&mut query.root));
    let mut result = Ok(());
    visit_root(&mut query.root, &mut |node| {
        if result.is_ok() {
            result = rewrite(node, &mut arena);
        }
    });
    *query_arena(&mut query.root) = arena;
    result
}

fn command_nodes(command: &mut CommandPlan, rewrite: Rewrite<'_>) -> Result<(), SQLError> {
    if let Some(ctes) = command.ctes_mut() {
        cte_nodes(ctes, rewrite)?;
    }
    if let Some(source) = command.source_input_mut() {
        source_nodes(source, rewrite)?;
    }
    for query in command.query_inputs_mut() {
        query_nodes(query, rewrite)?;
    }
    if let Some(arena) = command_arena(command) {
        let mut arena = std::mem::take(arena);
        let mut result = Ok(());
        let mut node = |node: &mut ScalarExpr| {
            if result.is_ok() {
                result = rewrite(node, &mut arena);
            }
        };
        for expression in command.expressions_mut() {
            crate::plan::rewrite_scalar_expression(expression, &mut node);
        }
        if let Some(source) = command.source_input_mut() {
            visit_source(source, &mut node);
        }
        *command_arena(command).expect("mutation arena") = arena;
        return result;
    }
    match command {
        CommandPlan::CreateView { query, .. }
        | CommandPlan::CreateMaterializedView { query, .. }
        | CommandPlan::CreateTableAs { query, .. }
        | CommandPlan::DeclareCursor { query, .. } => query_nodes(query, rewrite),
        CommandPlan::Explain { body, .. } | CommandPlan::Prepare { body, .. } => {
            rewrite_with_arenas(body, rewrite)
        }
        CommandPlan::Execute { params, .. } | CommandPlan::Call { args: params, .. } => {
            for expression in params {
                rewrite_expression_with_arena(
                    &mut expression.scalar,
                    &mut expression.subqueries,
                    rewrite,
                )?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn cte_nodes(ctes: &mut [CtePlan], rewrite: Rewrite<'_>) -> Result<(), SQLError> {
    for cte in ctes {
        match &mut cte.body {
            CtePlanBody::Query(query) => query_nodes(query, rewrite)?,
            CtePlanBody::Command(command) => command_nodes(command, rewrite)?,
        }
        if let Some(cycle) = &mut cte.cycle {
            rewrite_expression_with_arena(&mut cycle.mark_value, &mut Vec::new(), rewrite)?;
            rewrite_expression_with_arena(&mut cycle.mark_default, &mut Vec::new(), rewrite)?;
        }
    }
    Ok(())
}

fn source_nodes(source: &mut SourcePlan, rewrite: Rewrite<'_>) -> Result<(), SQLError> {
    match source {
        SourcePlan::Join { left, right, .. } => {
            source_nodes(left, rewrite)?;
            source_nodes(right, rewrite)
        }
        SourcePlan::Subquery { body, .. } => query_nodes(body, rewrite),
        _ => Ok(()),
    }
}

/// A copied occurrence initializes separately even when it has the same query text. The query arena remains the owner of the plan and its result-cache identity.
pub(crate) fn copy_occurrences(
    expression: &mut ScalarExpr,
    arena: &mut Vec<QueryPlan>,
) -> Result<(), SQLError> {
    let mut result = Ok(());
    crate::plan::rewrite_scalar_expression(expression, &mut |node| {
        if result.is_err() {
            return;
        }
        if let Some(slot) = subquery_id(node) {
            if let Some(query) = arena.get(*slot).cloned() {
                *slot = arena.len();
                arena.push(query);
            } else {
                result = Err(SQLError::Internal(
                    "copied scalar subquery is outside its owning arena".into(),
                ));
            }
        }
    });
    result
}
