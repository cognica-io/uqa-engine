//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Forward SQL's borrowed source metadata only while its native inputs are unchanged.

use crate::query::{scope::binding::SourceBindingScope, CteScope};
use std::sync::Arc;
use uqa_sql::{
    binding::{ExecutionQueryBlock, SharedExecutionSourceBindings},
    plan::{QueryBlockPlan, SourcePlan},
    routines::RoutineResolution,
    RowSchema, SQLError, SQLParam,
};

mod privileges;
#[cfg(test)]
mod tests;
pub(crate) use privileges::ensure_select_with_sources;
use privileges::SourceAuthorization;

pub(crate) struct BoundQueryBlock<'a> {
    pub(crate) block: ExecutionQueryBlock<'a>,
    pub(crate) schema: RowSchema,
    pub(crate) sources: Option<SourceBindings<'a>>,
}

#[derive(Clone)]
pub(crate) struct SourceBindings<'a> {
    source: SharedExecutionSourceBindings<'a>,
    scope: Arc<uqa_core::memory::Produced<SourceBindingScope>>,
    routines: &'a dyn RoutineResolution,
    params: &'a [SQLParam],
    authorization: Option<SourceAuthorization<'a>>,
}

impl SourceBindings<'_> {
    pub(crate) fn matches_source(&self, source: &SourcePlan) -> bool {
        self.source.matches_source(source)
    }

    pub(crate) fn join_child(&self, right: bool) -> Option<Self> {
        self.source.join_child(right).map(|source| Self {
            source,
            authorization: self
                .authorization
                .as_ref()
                .and_then(|checked| checked.join_child(right)),
            ..self.clone()
        })
    }

    fn query_source<S: Clone>(
        &self,
        routines: &dyn RoutineResolution,
        block: &QueryBlockPlan,
        params: &[SQLParam],
        ctes: &CteScope<S>,
        outer: Option<&RowSchema>,
        defer_distinct_limit: bool,
    ) -> Option<(RowSchema, Option<Self>)> {
        if !std::ptr::eq(self.routines, routines)
            || !std::ptr::eq(self.params, params)
            || !self.scope.matches(ctes)
        {
            return None;
        }
        self.source
            .query_source(block, outer, defer_distinct_limit)
            .map(|(schema, source)| {
                (
                    schema,
                    source.map(|source| Self {
                        source,
                        authorization: self
                            .authorization
                            .as_ref()
                            .and_then(|checked| checked.query_source(block, ctes)),
                        ..self.clone()
                    }),
                )
            })
    }
}

/// Rebind a correlated outer row only when the block introduces structural identities.
pub(crate) fn prepare_query_block<'a, S: Clone>(
    routines: &'a dyn RoutineResolution,
    block: &'a QueryBlockPlan,
    params: &'a [SQLParam],
    ctes: &mut CteScope<S>,
    inherited: Option<&SourceBindings<'a>>,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<BoundQueryBlock<'a>, SQLError> {
    if let Some(outer) = ctes.row_lock_outer_row() {
        if let std::borrow::Cow::Owned(rebound) = super::bind_outer_rewrite_row(block, outer) {
            ctes.set_row_lock_outer_row(rebound);
        }
    }
    let options = uqa_sql::binding::ExecutionBindingOptions {
        outer: ctes.row_lock_outer_row().map(|row| &row.schema),
        defer_distinct_limit: uqa_sql::semantics::should_defer_distinct_limit(block),
    };
    bind_query_block_with_sources(routines, block, params, ctes, options, inherited, control)
}

pub(crate) fn validate_block_set_contexts<S: Clone + Send + Sync + 'static>(
    context: &crate::query::relational::RelationalContext<'_, S>,
    block: &QueryBlockPlan,
    schema: &RowSchema,
    params: &[SQLParam],
    ctes: &CteScope<S>,
    outer: Option<&RowSchema>,
) -> Result<(), SQLError> {
    let resolver = context.expression_scope(ctes)?;
    uqa_sql::semantics::sets::validation::validate_query_set_contexts(
        context.catalog,
        resolver.as_ref(),
        block,
        schema,
        params,
    )?;
    if let Some(from) = &block.from {
        uqa_sql::semantics::sets::validation::validate_source_set_contexts_before_build(
            context.catalog,
            resolver.as_ref(),
            from,
            params,
            &super::binding_context(ctes)?,
            outer,
        )?;
    }
    Ok(())
}

/// Distinct bound and original query trees can each need view rewrites against the physical row type.
pub(crate) struct QueryBlockRewrites<'a, 'b> {
    bound: ExecutionQueryBlock<'a>,
    original: Option<ExecutionQueryBlock<'b>>,
}

impl<'a, 'b> QueryBlockRewrites<'a, 'b> {
    pub(crate) fn resolve(
        bound: ExecutionQueryBlock<'a>,
        original: &'b QueryBlockPlan,
        schema: &RowSchema,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let shared = std::ptr::eq(bound.as_ref(), original);
        let bound = bound.resolve_outer_rewrites(schema, control)?;
        let original = if shared {
            None
        } else {
            Some(ExecutionQueryBlock::borrowed(original).resolve_outer_rewrites(schema, control)?)
        };
        Ok(Self { bound, original })
    }

    pub(crate) fn blocks(&self) -> (&QueryBlockPlan, &QueryBlockPlan) {
        let bound = self.bound.as_ref();
        (bound, self.original.as_ref().map_or(bound, AsRef::as_ref))
    }
}

pub(crate) fn bind_query_block_with_sources<'a, S: Clone>(
    routines: &'a dyn RoutineResolution,
    block: &'a QueryBlockPlan,
    params: &'a [SQLParam],
    ctes: &CteScope<S>,
    options: uqa_sql::binding::ExecutionBindingOptions<'_>,
    inherited: Option<&SourceBindings<'a>>,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<BoundQueryBlock<'a>, SQLError> {
    control.check()?;
    let uqa_sql::binding::ExecutionBindingOptions {
        outer,
        defer_distinct_limit,
    } = options;
    if let Some((schema, sources)) = inherited.and_then(|sources| {
        sources.query_source(routines, block, params, ctes, outer, defer_distinct_limit)
    }) {
        return Ok(BoundQueryBlock {
            block: ExecutionQueryBlock::borrowed(block),
            schema,
            sources,
        });
    }
    let bound = uqa_sql::binding::bind_query_block_with_sources_with_control(
        routines,
        block,
        params,
        super::binding_context_with_control(ctes, control)?,
        options,
        control,
    )?;
    let sources = bound
        .sources
        .map(|source| -> Result<_, SQLError> {
            Ok(SourceBindings {
                source,
                scope: SourceBindingScope::capture(ctes, control)?,
                routines,
                params,
                authorization: None,
            })
        })
        .transpose()?;
    Ok(BoundQueryBlock {
        block: bound.block,
        schema: bound.schema,
        sources,
    })
}
