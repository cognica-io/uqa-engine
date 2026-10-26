//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Initialize arguments only for the physical sources the planner retains.

use crate::query::sources::context::SourcePlanning;
use uqa_sql::{plan::SourcePlan, SQLError, ScalarExpr};

pub(super) fn visit<'a, S: Clone>(
    planning: &dyn SourcePlanning<S>,
    source: &'a SourcePlan,
    consume: &mut dyn FnMut(&'a ScalarExpr) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    if planning.source_is_empty(source) {
        return Ok(());
    }
    match source {
        SourcePlan::Join {
            left, right, on, ..
        } => {
            if !planning.join_input_is_empty(source, false) {
                visit(planning, left, consume)?;
            }
            if !planning.join_input_is_empty(source, true) {
                visit(planning, right, consume)?;
            }
            if let Some(on) = on {
                consume(on)?;
            }
        }
        SourcePlan::Values { rows, .. } => {
            for expression in rows.iter().flatten() {
                consume(expression)?;
            }
        }
        SourcePlan::Function { args, .. } => {
            for expression in args {
                consume(expression)?;
            }
        }
        SourcePlan::FunctionGroup { functions, .. } => {
            for expression in functions.iter().flat_map(|function| &function.args) {
                consume(expression)?;
            }
        }
        // Derived and named queries initialize their own surviving expressions in their original scopes.
        SourcePlan::RetainedRows(_) | SourcePlan::Table { .. } | SourcePlan::Subquery { .. } => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests;
