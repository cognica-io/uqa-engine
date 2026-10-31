//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical window clauses own analyzed inputs; expanded call specifications are derived copies.

use super::{
    CommandPlan, CtePlan, CtePlanBody, QueryBlockPlan, QueryPlan, RelationalPlan, SourcePlan,
    UnifiedPlan,
};
use crate::{SQLError, ScalarExpr, ScalarWindowSpec};
mod production;
pub use production::resolved_window_definitions_with_control;

/// Resolve the canonical own clauses without evaluating any expression.
pub fn resolved_window_definitions(
    block: &QueryBlockPlan,
) -> Result<Vec<ScalarWindowSpec>, SQLError> {
    resolved_window_definitions_with_control(
        block,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )?
    .into_uncontrolled()
    .map_err(|_| unreachable!("ordinary canonical window owner"))
}

impl UnifiedPlan {
    /// Refresh execution copies after input constants or stored identities change. Call only after identity-based input replacement has completed.
    pub fn normalize_window_definitions(&mut self) -> Result<(), SQLError> {
        match self {
            Self::Query(query) => query.normalize_window_definitions(),
            Self::Command(command) => normalize_command(command),
        }
    }
}

impl QueryPlan {
    /// Keep each call's execution specification equal to its canonical query-local definition.
    pub fn normalize_window_definitions(&mut self) -> Result<(), SQLError> {
        normalize_ctes(&mut self.ctes)?;
        let result = match &mut self.root {
            RelationalPlan::QueryBlock(block) => {
                if let Some(source) = &mut block.from {
                    normalize_source(source)?;
                }
                for query in &mut block.subqueries {
                    query.normalize_window_definitions()?;
                }
                let resolved = resolved_window_definitions(block)?;
                let mut result = Ok(());
                let mut update = |node: &mut ScalarExpr| {
                    if result.is_ok() {
                        if let ScalarExpr::WindowCall { spec, .. } = node {
                            if let Some(slot) = spec.definition {
                                match resolved.get(slot) {
                                    Some(canonical) => spec.clone_from(canonical),
                                    None => {
                                        result = Err(invalid(
                                            "a call refers to a missing window definition",
                                        ));
                                    }
                                }
                            }
                        }
                    }
                };
                visit_local_roots_mut(block, &mut |expression| expression.visit_mut(&mut update));
                result
            }
            RelationalPlan::SetOp {
                left,
                right,
                subqueries,
                ..
            } => {
                left.normalize_window_definitions()?;
                right.normalize_window_definitions()?;
                for query in subqueries {
                    query.normalize_window_definitions()?;
                }
                Ok(())
            }
            RelationalPlan::Values { subqueries, .. } => {
                for query in subqueries {
                    query.normalize_window_definitions()?;
                }
                Ok(())
            }
        };
        result?;
        super::subqueries::prune_query(self);
        Ok(())
    }
}

fn normalize_command(command: &mut CommandPlan) -> Result<(), SQLError> {
    if let Some(ctes) = command.ctes_mut() {
        normalize_ctes(ctes)?;
    }
    if let Some(source) = command.source_input_mut() {
        normalize_source(source)?;
    }
    for query in command.query_inputs_mut() {
        query.normalize_window_definitions()?;
    }
    match command {
        CommandPlan::CreateView { query, .. }
        | CommandPlan::CreateMaterializedView { query, .. }
        | CommandPlan::CreateTableAs { query, .. }
        | CommandPlan::DeclareCursor { query, .. } => query.normalize_window_definitions(),
        CommandPlan::Explain { body, .. } | CommandPlan::Prepare { body, .. } => {
            body.normalize_window_definitions()
        }
        CommandPlan::Execute { params, .. } | CommandPlan::Call { args: params, .. } => {
            for expression in params {
                for query in &mut expression.subqueries {
                    query.normalize_window_definitions()?;
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn normalize_ctes(ctes: &mut [CtePlan]) -> Result<(), SQLError> {
    for cte in ctes {
        match &mut cte.body {
            CtePlanBody::Query(query) => query.normalize_window_definitions()?,
            CtePlanBody::Command(command) => normalize_command(command)?,
        }
    }
    Ok(())
}

fn normalize_source(source: &mut SourcePlan) -> Result<(), SQLError> {
    match source {
        SourcePlan::Subquery { body, .. } => body.normalize_window_definitions(),
        SourcePlan::Join { left, right, .. } => {
            normalize_source(left)?;
            normalize_source(right)
        }
        _ => Ok(()),
    }
}

fn visit_local_roots_mut(block: &mut QueryBlockPlan, visit: &mut dyn FnMut(&mut ScalarExpr)) {
    if let Some(source) = &mut block.from {
        visit_source_roots_mut(source, visit);
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
        visit(expression);
    }
}

fn visit_source_roots_mut(source: &mut SourcePlan, visit: &mut dyn FnMut(&mut ScalarExpr)) {
    match source {
        crate::plan::SourcePlan::RetainedRows(_) => {}
        SourcePlan::Table { .. } | SourcePlan::Subquery { .. } => {}
        SourcePlan::Join {
            left, right, on, ..
        } => {
            visit_source_roots_mut(left, visit);
            visit_source_roots_mut(right, visit);
            if let Some(on) = on {
                visit(on);
            }
        }
        SourcePlan::Values { rows, .. } => {
            for expression in rows.iter_mut().flatten() {
                visit(expression);
            }
        }
        SourcePlan::Function { args, .. } => {
            for expression in args {
                visit(expression);
            }
        }
        SourcePlan::FunctionGroup { functions, .. } => {
            for expression in functions.iter_mut().flat_map(|function| &mut function.args) {
                visit(expression);
            }
        }
    }
}

pub(in crate::plan) fn invalid(message: &str) -> SQLError {
    SQLError::Internal(format!("invalid stored window definitions: {message}"))
}

#[cfg(test)]
mod tests;
