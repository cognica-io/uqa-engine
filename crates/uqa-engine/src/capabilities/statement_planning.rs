//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Supply current catalog metadata and the configured retrieval planner to statement planning.
use crate::Engine;
use std::collections::BTreeMap;
use std::sync::Arc;
use uqa_core::catalog_index::CatalogIndexRow;
use uqa_planner::{
    statement_planning::{
        rule_inputs::{RuleInputColumns, RuleInputPlanningContext},
        PlannerStatisticsCatalog, RetrievalSourceCosting, StatementStatisticsContext,
        StatisticsTableState,
    },
    ColumnStats, LocalAccessEstimate,
};
use uqa_sql::{ast::OperatorJoinRelations, SQLError, ScalarExpr};

impl uqa_sql::plan::ExecutablePlanOptimizer for Engine {
    fn plan_for_execution(
        &self,
        plan: uqa_sql::plan::UnifiedPlan,
        params: &[uqa_sql::SQLParam],
    ) -> Result<uqa_sql::plan::UnifiedPlan, SQLError> {
        uqa_sql::plan::ExecutablePlanOptimizer::plan_for_execution(
            &self.statement_planning_context(),
            plan,
            params,
        )
    }
}

impl Engine {
    pub(crate) fn statement_statistics_context(&self) -> StatementStatisticsContext<'_> {
        StatementStatisticsContext {
            catalog: self,
            volatility: self,
            retrieval: self,
        }
    }
    pub(crate) fn rule_input_planning_context(&self) -> RuleInputPlanningContext<'_> {
        RuleInputPlanningContext {
            rules: self,
            views: self.view_rewrite_context(),
            columns: self,
        }
    }
}
impl StatisticsTableState for crate::TableState {}

impl PlannerStatisticsCatalog for Engine {
    fn storage_table(&self, table: &str) -> Result<Option<Arc<dyn StatisticsTableState>>, String> {
        self.try_table(table)
            .map(|state| state.map(|state| state as Arc<dyn StatisticsTableState>))
            .map_err(|error| error.to_string())
    }
    fn hierarchy_scan_tables(&self, table: &str) -> Result<Vec<String>, SQLError> {
        self.query_hierarchy_scan_tables(table, true)
    }
    fn table_row_count(&self, table: &str) -> Result<u64, SQLError> {
        self.table_doc_count(table)
    }
    fn column_statistics(&self, table: &str) -> Result<BTreeMap<String, ColumnStats>, String> {
        self.try_query_column_stats(table)
            .map_err(|error| error.to_string())
    }
    fn resolved_table_name(&self, table: &str) -> Result<Option<String>, SQLError> {
        self.resolve_table_name(table)
            .map_err(|error| SQLError::Internal(error.to_string()))
    }
    fn catalog_indexes(&self) -> Result<Vec<CatalogIndexRow>, SQLError> {
        self.list_catalog_indexes()
            .map_err(|error| SQLError::Internal(error.to_string()))
    }
}
impl RetrievalSourceCosting for Engine {
    fn operator_join_access(
        &self,
        name: &str,
        relations: Option<&OperatorJoinRelations>,
        args: &[ScalarExpr],
    ) -> Result<LocalAccessEstimate, SQLError> {
        crate::operator_tree_bridge::estimate_operator_join_table_function(
            self,
            name,
            relations,
            args,
            &[],
        )
    }
    fn local_access(
        &self,
        table: &str,
        predicate: &ScalarExpr,
    ) -> Result<Option<LocalAccessEstimate>, SQLError> {
        crate::operator_tree_bridge::estimate_local_access(self, table, predicate, &[])
    }
}
impl RuleInputColumns for Engine {
    fn source_column_names(
        &self,
        table: &str,
        relations_bound: bool,
    ) -> Result<Option<Vec<String>>, SQLError> {
        uqa_execution::catalog::projection::query_source_column_names(
            &self.catalog_execution(),
            table,
            relations_bound,
        )
    }
}

impl Engine {
    pub(crate) fn statement_planning_context(
        &self,
    ) -> uqa_planner::statement_planning::executable::StatementPlanningContext<'_> {
        uqa_planner::statement_planning::executable::StatementPlanningContext {
            analysis: uqa_sql::binding::statements::StatementAnalysisContext {
                scopes: self,
                routines: self,
            },
            aggregates: self,
            optimization: self,
            constant_evaluator: uqa_execution::scalar::eval_constant_scalar,
        }
    }
}
impl uqa_planner::statement_planning::executable::StatementOptimizationContexts for Engine {
    fn statistics(&self) -> StatementStatisticsContext<'_> {
        self.statement_statistics_context()
    }
    fn rule_inputs(&self) -> RuleInputPlanningContext<'_> {
        self.rule_input_planning_context()
    }
}
