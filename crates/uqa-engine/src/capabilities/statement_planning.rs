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
use uqa_execution::storage_errors::storage_error;
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
    fn plan_with_cached_analysis(
        &self,
        plan: uqa_sql::plan::UnifiedPlan,
        params: &[uqa_sql::SQLParam],
        cached: Option<Arc<uqa_sql::binding::statements::AnalyzedStatement>>,
    ) -> Result<
        (
            uqa_sql::plan::UnifiedPlan,
            Option<Arc<uqa_sql::binding::statements::AnalyzedStatement>>,
        ),
        SQLError,
    > {
        uqa_sql::plan::ExecutablePlanOptimizer::plan_with_cached_analysis(
            &self.statement_planning_context(),
            plan,
            params,
            cached,
        )
    }

    fn plan_for_statement_cache(
        &self,
        plan: uqa_sql::plan::UnifiedPlan,
        params: &[uqa_sql::SQLParam],
    ) -> Result<(uqa_sql::plan::UnifiedPlan, bool), SQLError> {
        uqa_sql::plan::ExecutablePlanOptimizer::plan_for_statement_cache(
            &self.statement_planning_context(),
            plan,
            params,
        )
    }

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

    fn plan_with_result(
        &self,
        plan: uqa_sql::plan::UnifiedPlan,
        params: &[uqa_sql::SQLParam],
    ) -> Result<
        (
            uqa_sql::plan::UnifiedPlan,
            uqa_sql::binding::statements::AnalyzedResult,
        ),
        SQLError,
    > {
        uqa_sql::plan::ExecutablePlanOptimizer::plan_with_result(
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
            volatility: self,
        }
    }
}
impl StatisticsTableState for crate::TableState {}

impl PlannerStatisticsCatalog for Engine {
    fn storage_table(
        &self,
        table: &str,
    ) -> Result<Option<Arc<dyn StatisticsTableState>>, SQLError> {
        self.try_table(table)
            .map(|state| state.map(|state| state as Arc<dyn StatisticsTableState>))
            .map_err(|error| {
                storage_error(
                    &format!("resolve optimizer storage table `{table}`"),
                    &error,
                )
            })
    }
    fn hierarchy_scan_tables(&self, table: &str) -> Result<Vec<String>, SQLError> {
        self.query_hierarchy_scan_tables(table, true)
    }
    fn table_row_count(&self, table: &str) -> Result<u64, SQLError> {
        self.table_row_estimate(table)
    }
    fn column_statistics(&self, table: &str) -> Result<BTreeMap<String, ColumnStats>, SQLError> {
        self.try_query_column_stats(table).map_err(|error| {
            storage_error(&format!("read optimizer statistics for `{table}`"), &error)
        })
    }
    fn resolved_table_name(&self, table: &str) -> Result<Option<String>, SQLError> {
        self.resolve_table_name(table)
            .map_err(|error| storage_error("resolve optimizer index target", &error))
    }
    fn catalog_indexes(&self) -> Result<Vec<CatalogIndexRow>, SQLError> {
        self.catalog_indexes_in_execution()
            .map_err(|error| storage_error("read optimizer catalog indexes", &error))
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
                aliases: self,
            },
            aggregates: self,
            optimization: self,
            constant_evaluator: uqa_execution::scalar::eval_constant_scalar_with_control,
        }
    }
}
impl uqa_planner::statement_planning::executable::StatementOptimizationContexts for Engine {
    fn production_resources(
        &self,
    ) -> Result<Option<uqa_planner::statement_planning::executable::PlanningResources>, SQLError>
    {
        let control = self.query_retention_control()?;
        Ok(Some(
            uqa_planner::statement_planning::executable::PlanningResources::new(
                control.memory().clone(),
                control.cancellation().clone(),
            ),
        ))
    }
    fn constant_storage(
        &self,
    ) -> Result<Option<Arc<dyn uqa_sql::ir::ConstantValueStorage>>, SQLError> {
        let bytes =
            uqa_execution::query::runtime::QueryMemorySettings::work_mem_bytes(&*self.session)?;
        Ok(Some(Arc::new(
            uqa_execution::scalar::RetainedConstants::new(bytes),
        )))
    }
    fn builtin_permissions(
        &self,
    ) -> Option<Arc<dyn uqa_sql::catalog::security::builtin_routines::BuiltinRoutineExecution>>
    {
        Some(Arc::new(uqa_execution::catalog::security::builtin_routines::execution::BuiltinRoutinePermissions::capture(&self.catalog_execution())))
    }
    fn statistics(&self) -> StatementStatisticsContext<'_> {
        self.statement_statistics_context()
    }
    fn rule_inputs(&self) -> RuleInputPlanningContext<'_> {
        self.rule_input_planning_context()
    }
    fn routine_inlining(&self) -> Option<uqa_sql::routines::inlining::RoutineInliningContext<'_>> {
        let compilation = self.routine_definition_context().compilation.analysis;
        Some(uqa_sql::routines::inlining::RoutineInliningContext {
            routines: compilation.routines,
            types: compilation.types,
            parsers: compilation.parsers,
            catalog: compilation.catalog,
            relations: compilation.relations,
            authority: self,
            volatility: self,
            expressions: self,
        })
    }
}

pub(crate) fn estimate_engine_plan(
    engine: &Engine,
    plan: &uqa_planner::UnifiedPlan,
) -> Result<uqa_planner::plan_cost::PlanCost, SQLError> {
    uqa_planner::statement_planning::executable::estimate_plan(
        &engine.statement_planning_context(),
        plan,
    )
}

pub(crate) fn optimize_engine_query(
    engine: &Engine,
    query: std::borrow::Cow<'_, uqa_planner::QueryPlan>,
) -> Result<uqa_planner::QueryPlan, SQLError> {
    uqa_planner::statement_planning::executable::optimize_query(
        &engine.statement_planning_context(),
        query,
    )
}

pub(crate) fn optimize_engine_plan(
    engine: &Engine,
    plan: uqa_planner::UnifiedPlan,
) -> Result<uqa_planner::UnifiedPlan, SQLError> {
    uqa_planner::statement_planning::executable::optimize_plan(
        &engine.statement_planning_context(),
        plan,
    )
}

impl uqa_sql::routines::inlining::RoutinePlanExpressions for Engine {
    fn evaluate_constant_routine(
        &self,
        expression: &ScalarExpr,
    ) -> Result<uqa_core::Value, SQLError> {
        uqa_execution::query::catalog_expression::eval_expression_plan_with_schema(
            self,
            crate::capabilities::query_scope::new_for_current_routine(self),
            uqa_sql::plan::ExpressionPlan {
                scalar: expression.clone(),
                subqueries: Vec::new(),
            },
            &uqa_sql::ResultRow::new(),
            &uqa_sql::RowSchema::default(),
            &[],
        )
    }
}
