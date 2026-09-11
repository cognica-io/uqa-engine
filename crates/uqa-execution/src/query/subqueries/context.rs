//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow query contexts only when an uncached subquery needs execution.

use crate::catalog::services::{CatalogSession, CatalogSnapshotSource};
use crate::query::{
    runtime::QueryMemorySettings, sources::SourceContext, statement::context::QueryContext,
    CteScope,
};
use crate::scalar::plan::PhysicalSubqueryRunner;
use uqa_sql::{expr::EngineHook, semantics::volatility::VolatilityCatalog, SQLError};

/// Bind native query services at the execution boundary, after cache and correlation checks.
pub trait SubqueryQueryContexts<S: Clone + 'static> {
    fn query_context(&self) -> QueryContext<'_, S>;
    fn source_context(&self) -> SourceContext<'_, S>;
}

pub type SubqueryProbe<'a> =
    &'a mut dyn FnMut(&dyn EngineHook, &dyn PhysicalSubqueryRunner) -> Result<bool, SQLError>;

/// Lend scoped callbacks to a physical predicate without allocating a callback object per row.
pub trait ScopedSubqueryHooks<S: Clone>: Sync {
    fn with_hooks(&self, scope: &CteScope<S>, probe: SubqueryProbe<'_>) -> Result<bool, SQLError>;
}

#[derive(Clone)]
pub struct SubqueryServices<'a, S: Clone + 'static> {
    pub catalog: &'a dyn CatalogSnapshotSource,
    pub session: &'a dyn CatalogSession,
    pub volatility: &'a dyn VolatilityCatalog,
    pub queries: &'a dyn SubqueryQueryContexts<S>,
    pub hooks: &'a dyn ScopedSubqueryHooks<S>,
}

impl<S: Clone + 'static> Copy for SubqueryServices<'_, S> {}

pub struct SubqueryContext<'a, S: Clone + 'static> {
    pub services: SubqueryServices<'a, S>,
    pub memory: &'a dyn QueryMemorySettings,
    pub ctes: &'a CteScope<S>,
    pub function_hook: &'a dyn EngineHook,
    pub subquery_runner: &'a dyn PhysicalSubqueryRunner,
}
