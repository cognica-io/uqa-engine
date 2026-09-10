//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Assemble independent capabilities for physical FROM execution.

use crate::{session::StatementReadSnapshot, Engine};
use uqa_execution::query::{
    sources::context::{ForeignSourceRows, ForeignTableScan, SourceContext, SourcePlanning},
    CteScope,
};
use uqa_sql::{
    plan::{
        source_projection::{ColumnPrune, QualifierFilters},
        QueryBlockPlan, SourcePlan,
    },
    SQLError, ScalarExpr,
};

impl Engine {
    pub(crate) fn source_execution_context(&self) -> SourceContext<'_, StatementReadSnapshot> {
        SourceContext {
            relational: self.relational_context(),
            ctes: self.cte_execution_context(),
            scans: self.table_scan_context(),
            retrieval: self.table_retrieval_context(),
            locking: self.row_lock_context(),
            catalog: self.catalog_execution(),
            planning: self,
            foreign_tables: self,
            volatility: self,
            types: self,
            text_indexes: self,
            documents: self,
            relation_retrieval: self,
        }
    }
}

impl SourcePlanning<StatementReadSnapshot> for Engine {
    fn column_prune_with_filter(
        &self,
        statement: &QueryBlockPlan,
        source: &SourcePlan,
        filter: Option<&ScalarExpr>,
        scope: &CteScope<StatementReadSnapshot>,
    ) -> Result<Option<ColumnPrune>, SQLError> {
        super::query_planning::column_prune_for_stmt_with_filter(
            self, statement, source, filter, scope,
        )
    }
    fn column_prune(
        &self,
        statement: &QueryBlockPlan,
        source: &SourcePlan,
        scope: &CteScope<StatementReadSnapshot>,
    ) -> Result<Option<ColumnPrune>, SQLError> {
        super::query_planning::column_prune_for_stmt(self, statement, source, scope)
    }
    fn qualifier_filters(
        &self,
        statement: &QueryBlockPlan,
        source: &SourcePlan,
        scope: &CteScope<StatementReadSnapshot>,
    ) -> Result<Option<QualifierFilters>, SQLError> {
        super::query_planning::qualifier_filters_for_stmt(self, statement, source, scope)
    }
    fn residual_filter(
        &self,
        statement: &QueryBlockPlan,
        source: &SourcePlan,
        filters: Option<&QualifierFilters>,
        scope: &CteScope<StatementReadSnapshot>,
    ) -> Result<Option<ScalarExpr>, SQLError> {
        super::query_planning::final_filter_after_qualifier_pushdown(
            self, statement, source, filters, scope,
        )
    }
    fn propagated_join_filters(
        &self,
        filters: &QualifierFilters,
        source: &SourcePlan,
        target: &SourcePlan,
        on: Option<&ScalarExpr>,
    ) -> Option<QualifierFilters> {
        uqa_planner::source_filters::propagated_join_filters(filters, source, target, on)
    }
}

impl ForeignTableScan for Engine {
    fn scan_foreign_source(
        &self,
        name: &str,
        predicates: &[uqa_fdw::FDWPredicate],
    ) -> Result<ForeignSourceRows<'_>, String> {
        self.scan_foreign_table_stream(name, None, predicates, None)
    }
}
