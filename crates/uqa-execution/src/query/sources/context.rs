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
    ResultRow, SQLError, SQLParam, ScalarExpr,
};

/// Planner decisions needed when a parameterized source is assembled for execution.
pub trait SourcePlanning<S: Clone>: Sync {
    /// Whether the planner removes this source's physical scan. The default keeps initialization for planners without this proof.
    fn source_is_empty(&self, _source: &SourcePlan) -> bool {
        false
    }
    /// Whether a block keeps its output operators but removes its FROM input.
    fn block_input_is_empty(&self, _statement: &QueryBlockPlan) -> bool {
        false
    }
    /// Whether an outer join removes one matching input while retaining its preserved side.
    fn join_input_is_empty(&self, _join: &SourcePlan, _right: bool) -> bool {
        false
    }
    fn column_prune_with_filter(
        &self,
        statement: &QueryBlockPlan,
        source: &SourcePlan,
        filter: Option<&ScalarExpr>,
        scope: &CteScope<S>,
        schema: Option<&crate::RowSchema>,
    ) -> Result<Option<ColumnPrune>, SQLError>;
    fn column_prune(
        &self,
        statement: &QueryBlockPlan,
        source: &SourcePlan,
        scope: &CteScope<S>,
        schema: Option<&crate::RowSchema>,
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
    ) -> Result<Option<uqa_sql::plan::OwnedScalarExpr>, SQLError>;
    fn propagated_join_filters(
        &self,
        filters: &QualifierFilters,
        source: &SourcePlan,
        target: &SourcePlan,
        on: Option<&ScalarExpr>,
    ) -> Result<Option<QualifierFilters>, SQLError>;
}

pub type ForeignSourceRows<'a> = Box<dyn Iterator<Item = Result<ResultRow, String>> + Send + 'a>;

/// A foreign table scan bound to the caller's catalog generation and FDW registrations.
pub trait ForeignTableScan: Sync {
    fn scan_foreign_source(
        &self,
        name: &str,
        filter: Option<&ScalarExpr>,
        params: &[SQLParam],
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
    pub foreign_tables: Option<&'a dyn ForeignTableScan>,
    pub volatility: &'a (dyn VolatilityCatalog + Sync),
    pub types: &'a dyn FunctionTypeResolver,
    pub documents: &'a dyn crate::query::block::context::QueryDocumentRead,
    pub relation_retrieval: &'a dyn crate::query::block::context::RelationRetrieval,
    pub text_indexes: &'a (dyn uqa_sql::semantics::text_indexes::TextMatchCatalog + Sync),
}
impl<S: Clone + 'static> Copy for SourceContext<'_, S> {}

impl<'a, S: Clone + 'static> SourceContext<'a, S> {
    pub fn scan_foreign_source(
        &self,
        name: &str,
        filter: Option<&ScalarExpr>,
        params: &[SQLParam],
    ) -> Result<ForeignSourceRows<'a>, String> {
        self.foreign_tables
            .ok_or_else(|| "FDW support is not available in this build".to_owned())?
            .scan_foreign_source(name, filter, params)
    }
}
