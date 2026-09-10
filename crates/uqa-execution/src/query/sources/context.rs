//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Inputs for source planning, scans, expression binding, and nested query execution.

use crate::query::{
    cte::context::CteExecutionContext,
    locking::context::RowLockContext,
    relational::RelationalContext,
    table_sources::{TableRetrievalContext, TableScanContext},
    CteScope,
};
use crate::{catalog::context::CatalogContext, FunctionTypeResolver};
use uqa_sql::{
    plan::{
        source_projection::{ColumnPrune, QualifierFilters},
        QueryBlockPlan, SourcePlan,
    },
    semantics::volatility::VolatilityCatalog,
    ResultRow, SQLError, ScalarExpr,
};

/// Planner decisions needed when a parameterized source is assembled for execution.
pub trait SourcePlanning<S: Clone>: Sync {
    fn column_prune_with_filter(
        &self,
        statement: &QueryBlockPlan,
        source: &SourcePlan,
        filter: Option<&ScalarExpr>,
        scope: &CteScope<S>,
    ) -> Result<Option<ColumnPrune>, SQLError>;
    fn column_prune(
        &self,
        statement: &QueryBlockPlan,
        source: &SourcePlan,
        scope: &CteScope<S>,
    ) -> Result<Option<ColumnPrune>, SQLError>;
    fn qualifier_filters(
        &self,
        statement: &QueryBlockPlan,
        source: &SourcePlan,
        scope: &CteScope<S>,
    ) -> Result<Option<QualifierFilters>, SQLError>;
    fn residual_filter(
        &self,
        statement: &QueryBlockPlan,
        source: &SourcePlan,
        filters: Option<&QualifierFilters>,
        scope: &CteScope<S>,
    ) -> Result<Option<ScalarExpr>, SQLError>;
    fn propagated_join_filters(
        &self,
        filters: &QualifierFilters,
        source: &SourcePlan,
        target: &SourcePlan,
        on: Option<&ScalarExpr>,
    ) -> Option<QualifierFilters>;
}

pub type ForeignSourceRows<'a> = Box<dyn Iterator<Item = Result<ResultRow, String>> + Send + 'a>;

/// A foreign table scan bound to the caller's catalog generation and FDW registrations.
pub trait ForeignTableScan: Sync {
    fn scan_foreign_source(
        &self,
        name: &str,
        predicates: &[uqa_fdw::FDWPredicate],
    ) -> Result<ForeignSourceRows<'_>, String>;
}

#[derive(Clone)]
pub struct SourceContext<'a, S: Clone + 'static> {
    pub relational: RelationalContext<'a, S>,
    pub ctes: CteExecutionContext<'a, S>,
    pub scans: TableScanContext<'a>,
    pub retrieval: TableRetrievalContext<'a>,
    pub locking: RowLockContext<'a, S>,
    pub catalog: CatalogContext<'a>,
    pub planning: &'a dyn SourcePlanning<S>,
    pub foreign_tables: &'a dyn ForeignTableScan,
    pub volatility: &'a (dyn VolatilityCatalog + Sync),
    pub types: &'a dyn FunctionTypeResolver,
    pub documents: &'a dyn crate::query::block::context::QueryDocumentRead,
    pub relation_retrieval: &'a dyn crate::query::block::context::RelationRetrieval,
    pub text_indexes: &'a (dyn uqa_sql::semantics::text_indexes::TextMatchCatalog + Sync),
}
impl<S: Clone + 'static> Copy for SourceContext<'_, S> {}
