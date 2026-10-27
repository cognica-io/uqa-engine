//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execution-local source bindings without copying unchanged query syntax.

use std::borrow::Cow;
use std::sync::Arc;

use super::{bind_source_plan_schema_for_execution, BindingContext};
use crate::{
    plan::{QueryBlockPlan, SourcePlan},
    routines::RoutineResolution,
    RowSchema, SQLError, SQLParam,
};

mod sources;
pub(in crate::binding) use sources::CapturedSources;
pub use sources::ExecutionSourceBindings;

pub struct BoundExecutionBlock<'a> {
    pub block: Cow<'a, QueryBlockPlan>,
    pub schema: RowSchema,
    pub sources: Option<Arc<ExecutionSourceBindings<'a>>>,
}

/// Bind the current execution tree and retain its already visited derived sources for immediate physical assembly in this same immutable scope. Source/function rewrites retain their ordinary binding path.
pub fn bind_query_block_with_sources<'a>(
    routines: &dyn RoutineResolution,
    block: &'a QueryBlockPlan,
    params: &[SQLParam],
    context: &BindingContext<'_>,
    outer: Option<&RowSchema>,
    defer_distinct_limit: bool,
) -> Result<BoundExecutionBlock<'a>, SQLError> {
    bind_execution_block(
        routines,
        block,
        params,
        context,
        outer,
        defer_distinct_limit,
        true,
    )
}

/// Keep the input immutable while selecting table-function identities and, when requested, moving the DISTINCT slice to its existing later boundary.
pub fn bind_query_block_for_execution<'a>(
    routines: &dyn RoutineResolution,
    block: &'a QueryBlockPlan,
    params: &[SQLParam],
    context: &BindingContext<'_>,
    outer: Option<&RowSchema>,
    defer_distinct_limit: bool,
) -> Result<(Cow<'a, QueryBlockPlan>, RowSchema), SQLError> {
    let bound = bind_execution_block(
        routines,
        block,
        params,
        context,
        outer,
        defer_distinct_limit,
        false,
    )?;
    Ok((bound.block, bound.schema))
}

fn bind_execution_block<'a>(
    routines: &dyn RoutineResolution,
    block: &'a QueryBlockPlan,
    params: &[SQLParam],
    context: &BindingContext<'_>,
    outer: Option<&RowSchema>,
    defer_distinct_limit: bool,
    retain_sources: bool,
) -> Result<BoundExecutionBlock<'a>, SQLError> {
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
        return Ok(BoundExecutionBlock {
            block: Cow::Owned(execution),
            schema: source,
            sources: None,
        });
    }
    let (schema, sources) = if let Some(source) = &block.from {
        let mut scope = super::SchemaScope::from_context(context)?;
        scope.execution_sources = retain_sources
            .then(|| CapturedSources::for_source(source))
            .filter(|captured| !captured.is_empty());
        let schema =
            scope.bind_source(routines, source, context.scalar_subqueries, params, outer)?;
        let sources = scope
            .execution_sources
            .as_mut()
            .map(|captured| captured.take(source));
        (schema, sources)
    } else {
        (RowSchema::default(), None)
    };
    Ok(BoundExecutionBlock {
        block: crate::semantics::view_rewrite::resolve_outer_rewrites(block, &schema),
        schema,
        sources,
    })
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
