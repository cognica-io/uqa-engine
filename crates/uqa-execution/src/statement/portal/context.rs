//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow live portal registries and defer native query and RETURNING inputs.

use crate::{
    catalog::services::CatalogSession,
    mutation::{command_scope::CommandScopeSource, returning::ReturningExecutionContext},
    statement::context::{
        queries::StatementQueryContexts, session::StatementPortals, ExplainRenderer,
    },
};
use uqa_sql::{
    routines::{
        declaration::RoutineTypeCatalog, resolution::RoutineOverloadCatalog, RoutineResolution,
    },
    semantics::returning::ReturningAnalysisContext,
    SQLParam,
};

pub trait PortalReturningInputs<S: Clone + 'static> {
    fn returning_execution_context(&self) -> ReturningExecutionContext<'_, S>;
    fn returning_analysis_context(&self) -> ReturningAnalysisContext<'_>;
}

#[derive(Clone)]
pub struct PortalExecutionContext<'a, S: Clone + 'static> {
    pub state: &'a dyn StatementPortals,
    pub queries: &'a dyn StatementQueryContexts<S>,
    pub returning: &'a dyn PortalReturningInputs<S>,
    pub command_scopes: &'a dyn CommandScopeSource<S>,
    pub routines: &'a dyn RoutineResolution,
    pub overloads: &'a dyn RoutineOverloadCatalog,
    pub types: &'a dyn RoutineTypeCatalog,
    pub session: &'a dyn CatalogSession,
    pub explain: ExplainRenderer,
    pub source_sql: Option<&'a str>,
    pub created_at_micros: i64,
}

impl<'a, S: Clone + 'static> PortalExecutionContext<'a, S> {
    pub fn with_source_sql(mut self, source_sql: Option<&'a str>) -> Self {
        self.source_sql = source_sql;
        self
    }
}

pub struct SessionPortalDeclaration {
    pub metadata: uqa_sql::catalog::session::CursorMetadata,
    pub query: uqa_sql::plan::QueryPlan,
    pub params: Vec<SQLParam>,
    pub columns: Vec<String>,
    pub column_types: Vec<Option<uqa_sql::ast::ColumnType>>,
}

pub struct SessionPortalCommandDeclaration {
    pub metadata: uqa_sql::catalog::session::CursorMetadata,
    pub command: Box<uqa_sql::plan::CommandPlan>,
    pub params: Vec<SQLParam>,
    pub columns: Vec<String>,
    pub column_types: Vec<Option<uqa_sql::ast::ColumnType>>,
    /// `PostgreSQL` 18 materializes one `NULL`-filled tuple for each row produced by a modifying command opened with explicit `SCROLL`.
    pub null_returning_values: bool,
}
