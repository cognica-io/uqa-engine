//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Forward SQL's borrowed source metadata only while its native inputs are unchanged.

use crate::query::{scope::binding::SourceBindingScope, CteScope};
use std::{borrow::Cow, sync::Arc};
use uqa_sql::{
    binding::ExecutionSourceBindings,
    plan::{QueryBlockPlan, SourcePlan},
    routines::RoutineResolution,
    RowSchema, SQLError, SQLParam,
};

#[cfg(test)]
mod tests;

pub(crate) struct BoundQueryBlock<'a> {
    pub(crate) block: Cow<'a, QueryBlockPlan>,
    pub(crate) schema: RowSchema,
    pub(crate) sources: Option<SourceBindings<'a>>,
}

#[derive(Clone)]
pub(crate) struct SourceBindings<'a> {
    source: Arc<ExecutionSourceBindings<'a>>,
    scope: Arc<SourceBindingScope>,
    routines: &'a dyn RoutineResolution,
    params: &'a [SQLParam],
}

impl SourceBindings<'_> {
    pub(crate) fn matches_source(&self, source: &SourcePlan) -> bool {
        self.source.matches_source(source)
    }

    pub(crate) fn join_child(&self, right: bool) -> Option<Self> {
        self.source.join_child(right).map(|source| Self {
            source,
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
                        ..self.clone()
                    }),
                )
            })
    }
}

pub(crate) fn bind_query_block_with_sources<'a, S: Clone>(
    routines: &'a dyn RoutineResolution,
    block: &'a QueryBlockPlan,
    params: &'a [SQLParam],
    ctes: &CteScope<S>,
    outer: Option<&RowSchema>,
    defer_distinct_limit: bool,
    inherited: Option<&SourceBindings<'a>>,
) -> Result<BoundQueryBlock<'a>, SQLError> {
    if let Some((schema, sources)) = inherited.and_then(|sources| {
        sources.query_source(routines, block, params, ctes, outer, defer_distinct_limit)
    }) {
        return Ok(BoundQueryBlock {
            block: Cow::Borrowed(block),
            schema,
            sources,
        });
    }
    let bound = uqa_sql::binding::bind_query_block_with_sources(
        routines,
        block,
        params,
        &super::binding_context(ctes)?,
        outer,
        defer_distinct_limit,
    )?;
    let sources = bound
        .sources
        .map(|source| -> Result<_, SQLError> {
            Ok(SourceBindings {
                source,
                scope: Arc::new(SourceBindingScope::capture(ctes)?),
                routines,
                params,
            })
        })
        .transpose()?;
    Ok(BoundQueryBlock {
        block: bound.block,
        schema: bound.schema,
        sources,
    })
}
