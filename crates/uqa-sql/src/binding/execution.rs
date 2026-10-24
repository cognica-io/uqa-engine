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
    let captured_outer =
        outer.map(|schema| crate::semantics::view_rewrite::bind_outer_rewrite_scope(block, schema));
    let outer = captured_outer.as_deref();
    if defer_distinct_limit
        || block.from.as_ref().is_some_and(source_binds_routines)
        || !block.deferred_outer_rewrites.is_empty() && block.from.is_some()
    {
        let mut execution = crate::semantics::select_execution_stmt(block, defer_distinct_limit);
        if let Cow::Owned(rewritten) = crate::semantics::view_rewrite::resolve_outer_sources(
            &execution,
            outer,
            &mut |source, subqueries, scope| {
                super::sources::bind_source_input_schema(
                    routines,
                    source,
                    params,
                    &BindingContext {
                        scalar_subqueries: subqueries,
                        ..context.clone()
                    },
                    scope,
                )
            },
        )? {
            execution = rewritten;
        }
        let source_context = BindingContext {
            scalar_subqueries: &execution.subqueries,
            ..context.clone()
        };
        let source = execution.from.as_mut().map_or_else(
            || Ok(RowSchema::default()),
            |source| {
                bind_source_plan_schema_for_execution(
                    routines,
                    source,
                    params,
                    &source_context,
                    outer,
                )
            },
        )?;
        if let Cow::Owned(rewritten) =
            crate::semantics::view_rewrite::resolve_outer_rewrites(&execution, &source)
        {
            execution = rewritten;
        }
        return Ok((Cow::Owned(execution), source));
    }
    let source = block.from.as_ref().map_or_else(
        || Ok(RowSchema::default()),
        |source| bind_source_plan_schema(routines, source, params, context, outer),
    )?;
    Ok((
        crate::semantics::view_rewrite::resolve_outer_rewrites(block, &source),
        source,
    ))
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
