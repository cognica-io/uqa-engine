//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execution-local source bindings without copying unchanged query syntax.

use std::borrow::Cow;

use super::{bind_source_plan_schema, bind_source_plan_schema_for_execution, BindingContext};
use crate::{
    plan::{QueryBlockPlan, SourcePlan},
    routines::RoutineResolution,
    RowSchema, SQLError, SQLParam,
};

/// Keep the input immutable while selecting table-function identities and, when requested, moving the DISTINCT slice to its existing later boundary.
pub fn bind_query_block_for_execution<'a>(
    routines: &dyn RoutineResolution,
    block: &'a QueryBlockPlan,
    params: &[SQLParam],
    context: &BindingContext<'_>,
    outer: Option<&RowSchema>,
    defer_distinct_limit: bool,
) -> Result<(Cow<'a, QueryBlockPlan>, RowSchema), SQLError> {
    if defer_distinct_limit || block.from.as_ref().is_some_and(source_binds_routines) {
        let mut execution = crate::semantics::select_execution_stmt(block, defer_distinct_limit);
        let source = execution.from.as_mut().map_or_else(
            || Ok(RowSchema::default()),
            |source| {
                bind_source_plan_schema_for_execution(routines, source, params, context, outer)
            },
        )?;
        return Ok((Cow::Owned(execution), source));
    }
    let source = block.from.as_ref().map_or_else(
        || Ok(RowSchema::default()),
        |source| bind_source_plan_schema(routines, source, params, context, outer),
    )?;
    Ok((Cow::Borrowed(block), source))
}

fn source_binds_routines(source: &SourcePlan) -> bool {
    match source {
        SourcePlan::Function { .. } | SourcePlan::FunctionGroup { .. } => true,
        SourcePlan::Join { left, right, .. } => {
            source_binds_routines(left) || source_binds_routines(right)
        }
        // A nested query selects its routines when its own execution block is entered.
        SourcePlan::Table { .. }
        | SourcePlan::Subquery { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::RetainedRows(_) => false,
    }
}

#[cfg(test)]
mod tests;
