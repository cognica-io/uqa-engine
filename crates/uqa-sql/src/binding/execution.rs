//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execution-local source bindings preserve borrowed syntax or an admitted private query.

use super::{production::ControlledBindingContext, BindingContext, SchemaScope};
use crate::{
    plan::{QueryBlockPlan, RelationalPlan, SourcePlan},
    routines::RoutineResolution,
    RowSchema, SQLError, SQLParam,
};
use std::borrow::Cow;
use uqa_core::memory::ProductionControl;

mod block;
mod sources;
pub use block::ExecutionQueryBlock;
pub(in crate::binding) use sources::CapturedSources;
pub use sources::{ExecutionSourceBindings, SharedExecutionSourceBindings};

#[derive(Clone, Copy, Default)]
pub struct ExecutionBindingOptions<'a> {
    pub outer: Option<&'a RowSchema>,
    pub defer_distinct_limit: bool,
}

pub struct BoundExecutionBlock<'a> {
    pub block: Cow<'a, QueryBlockPlan>,
    pub schema: RowSchema,
    pub sources: Option<SharedExecutionSourceBindings<'a>>,
}

pub struct ControlledExecutionBlock<'a> {
    pub block: ExecutionQueryBlock<'a>,
    pub schema: RowSchema,
    pub sources: Option<SharedExecutionSourceBindings<'a>>,
}

/// Bind the current execution tree and retain its already visited derived sources for immediate physical assembly in this same immutable scope.
pub fn bind_query_block_with_sources<'a>(
    routines: &dyn RoutineResolution,
    block: &'a QueryBlockPlan,
    params: &[SQLParam],
    context: &BindingContext<'_>,
    outer: Option<&RowSchema>,
    defer_distinct_limit: bool,
) -> Result<BoundExecutionBlock<'a>, SQLError> {
    let control = ProductionControl::uncontrolled();
    let bound = bind_execution_block(
        routines,
        block,
        params,
        context.copy_with_control(&control)?,
        ExecutionBindingOptions {
            outer,
            defer_distinct_limit,
        },
        true,
        &control,
    )?;
    Ok(BoundExecutionBlock {
        block: bound.block.into_uncontrolled(),
        schema: bound.schema,
        sources: bound.sources,
    })
}

/// Keep the input immutable while selecting table-function identities and moving DISTINCT slicing to its existing later boundary.
pub fn bind_query_block_for_execution<'a>(
    routines: &dyn RoutineResolution,
    block: &'a QueryBlockPlan,
    params: &[SQLParam],
    context: &BindingContext<'_>,
    outer: Option<&RowSchema>,
    defer_distinct_limit: bool,
) -> Result<(Cow<'a, QueryBlockPlan>, RowSchema), SQLError> {
    let control = ProductionControl::uncontrolled();
    let bound = bind_execution_block(
        routines,
        block,
        params,
        context.copy_with_control(&control)?,
        ExecutionBindingOptions {
            outer,
            defer_distinct_limit,
        },
        false,
        &control,
    )?;
    Ok((bound.block.into_uncontrolled(), bound.schema))
}

/// Bind a private execution block without retaining descriptors for recursive physical source transfer.
pub fn bind_query_block_for_execution_with_control<'a>(
    routines: &dyn RoutineResolution,
    block: &'a QueryBlockPlan,
    params: &[SQLParam],
    context: ControlledBindingContext<'_>,
    options: ExecutionBindingOptions<'_>,
    control: &ProductionControl<'_>,
) -> Result<(ExecutionQueryBlock<'a>, RowSchema), SQLError> {
    let bound = bind_execution_block(routines, block, params, context, options, false, control)?;
    Ok((bound.block, bound.schema))
}

pub fn bind_query_block_with_sources_with_control<'a>(
    routines: &dyn RoutineResolution,
    block: &'a QueryBlockPlan,
    params: &[SQLParam],
    context: ControlledBindingContext<'_>,
    options: ExecutionBindingOptions<'_>,
    control: &ProductionControl<'_>,
) -> Result<ControlledExecutionBlock<'a>, SQLError> {
    bind_execution_block(routines, block, params, context, options, true, control)
}

fn bind_execution_block<'a>(
    routines: &dyn RoutineResolution,
    block: &'a QueryBlockPlan,
    params: &[SQLParam],
    context: ControlledBindingContext<'_>,
    options: ExecutionBindingOptions<'_>,
    retain_sources: bool,
    control: &ProductionControl<'_>,
) -> Result<ControlledExecutionBlock<'a>, SQLError> {
    control.check()?;
    let captured_outer = options
        .outer
        .map(|schema| {
            crate::semantics::view_rewrite::bind_outer_rewrite_scope_with_control(
                block, schema, control,
            )
        })
        .transpose()?;
    let outer = captured_outer.as_deref();
    if options.defer_distinct_limit
        || block.from.as_ref().is_some_and(source_binds_routines)
        || !block.deferred_outer_rewrites.is_empty() && block.from.is_some()
    {
        return bind_rewritten_block(
            routines,
            block,
            params,
            context,
            ExecutionBindingOptions { outer, ..options },
            control,
        );
    }
    let (schema, sources) = if let Some(source) = &block.from {
        let subqueries = context.scalar_subqueries;
        let mut scope = SchemaScope::from_context_with_control(context, control)?;
        if retain_sources {
            let captured = CapturedSources::for_source(source, control)?;
            scope.execution_sources = (!captured.is_empty()).then_some(captured);
        }
        let schema = scope.bind_source(routines, source, subqueries, params, outer)?;
        let sources = scope
            .execution_sources
            .as_mut()
            .map(|captured| captured.take(source, control))
            .transpose()?;
        (schema, sources)
    } else {
        (empty_schema(control)?, None)
    };
    let block = ExecutionQueryBlock::borrowed(block).resolve_outer_rewrites(&schema, control)?;
    Ok(ControlledExecutionBlock {
        block,
        schema,
        sources,
    })
}

fn bind_rewritten_block<'a>(
    routines: &dyn RoutineResolution,
    block: &'a QueryBlockPlan,
    params: &[SQLParam],
    context: ControlledBindingContext<'_>,
    options: ExecutionBindingOptions<'_>,
    control: &ProductionControl<'_>,
) -> Result<ControlledExecutionBlock<'a>, SQLError> {
    let outer = options.outer;
    let mut execution = block.copy_for_rewrite(control)?;
    if options.defer_distinct_limit {
        execution = execution.clear_block_slice(control)?;
    }
    execution = crate::semantics::view_rewrite::resolve_outer_sources_owned(
        execution,
        outer,
        &mut |source, subqueries, scope| {
            let context = ControlledBindingContext {
                scalar_subqueries: subqueries,
                ..context.copy_with_control(control)?
            };
            SchemaScope::from_context_with_control(context, control)?
                .bind_source_input(routines, source, subqueries, params, scope)
        },
        control,
    )?;
    let mut schema = None;
    execution = execution.map_source_with_parent(control, |parent, source| {
        let RelationalPlan::QueryBlock(parent) = &parent.root else {
            unreachable!("original query block")
        };
        let context = ControlledBindingContext {
            scalar_subqueries: &parent.subqueries,
            ..context.copy_with_control(control)?
        };
        let (source, bound) = SchemaScope::from_context_with_control(context, control)?
            .bind_owned_source_for_execution(routines, source, &parent.subqueries, params, outer)?;
        schema = Some(bound);
        Ok(source)
    })?;
    let schema = match schema {
        Some(schema) => schema,
        None => empty_schema(control)?,
    };
    execution =
        crate::semantics::view_rewrite::resolve_outer_rewrites_owned(execution, &schema, control)?;
    Ok(ControlledExecutionBlock {
        block: ExecutionQueryBlock::owned(execution, control)?,
        schema,
        sources: None,
    })
}

fn empty_schema(control: &ProductionControl<'_>) -> Result<RowSchema, SQLError> {
    RowSchema::new_with_control(
        control.finish(Vec::new(), control.empty_reservation())?,
        control,
    )
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
