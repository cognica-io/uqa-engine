//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Initialize arguments only for the physical sources the planner retains.

use crate::query::sources::context::SourcePlanning;
use uqa_sql::{plan::SourcePlan, ScalarExpr};

pub(super) fn collect<'a, S: Clone>(
    planning: &dyn SourcePlanning<S>,
    source: &'a SourcePlan,
    output: &mut Vec<&'a ScalarExpr>,
) {
    if planning.source_is_empty(source) {
        return;
    }
    match source {
        SourcePlan::Join {
            left, right, on, ..
        } => {
            if !planning.join_input_is_empty(source, false) {
                collect(planning, left, output);
            }
            if !planning.join_input_is_empty(source, true) {
                collect(planning, right, output);
            }
            output.extend(on);
        }
        SourcePlan::Values { rows, .. } => output.extend(rows.iter().flatten()),
        SourcePlan::Function { args, .. } => output.extend(args),
        SourcePlan::FunctionGroup { functions, .. } => {
            output.extend(functions.iter().flat_map(|function| &function.args));
        }
        // Derived and named queries initialize their own surviving expressions in their original scopes.
        SourcePlan::RetainedRows(_) | SourcePlan::Table { .. } | SourcePlan::Subquery { .. } => {}
    }
}

#[cfg(test)]
mod tests;
