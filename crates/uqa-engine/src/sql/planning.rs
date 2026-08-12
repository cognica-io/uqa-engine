//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{volatility, Engine, SQLError, SQLParam, SQLResult, Statement, UnifiedPlanExecutor};

#[cfg(test)]
use super::{compile, Arc};

struct EngineSourceStatistics<'a> {
    engine: &'a Engine,
    error: &'a std::cell::RefCell<Option<SQLError>>,
}

impl EngineSourceStatistics<'_> {
    fn record_error(&self, error: SQLError) {
        if self.error.borrow().is_none() {
            *self.error.borrow_mut() = Some(error);
        }
    }
}

impl uqa_planner::SourceStatistics for EngineSourceStatistics<'_> {
    fn relation_statistics(&self, table: &str) -> Option<uqa_planner::RelationStats> {
        match self.engine.try_table(table) {
            Ok(None) => None,
            Ok(Some(_)) => match (
                self.engine.table_doc_count(table),
                self.engine.try_column_stats(table),
            ) {
                (Ok(row_count), Ok(columns)) => {
                    Some(uqa_planner::RelationStats { row_count, columns })
                }
                (Err(error), _) => {
                    self.record_error(error);
                    None
                }
                (_, Err(error)) => {
                    self.record_error(SQLError::Internal(format!(
                        "read optimizer statistics for `{table}`: {error}"
                    )));
                    None
                }
            },
            Err(error) => {
                self.record_error(SQLError::Internal(format!(
                    "resolve optimizer storage table `{table}`: {error}"
                )));
                None
            }
        }
    }

    fn source_access_estimate(
        &self,
        source: &uqa_planner::SourcePlan,
    ) -> Option<uqa_planner::LocalAccessEstimate> {
        let uqa_planner::SourcePlan::Function { name, args, .. } = source else {
            return None;
        };
        if args
            .iter()
            .any(uqa_execution::ScalarExpr::contains_parameter)
        {
            return None;
        }
        let identity = name.to_ascii_lowercase();
        let lower = crate::sql::builtin_function_dispatch_name(&identity);
        if !crate::operator_tree_bridge::is_operator_join_table_function(&lower) {
            return None;
        }
        match crate::operator_tree_bridge::estimate_operator_join_table_function(
            self.engine,
            &lower,
            args,
            &[],
        ) {
            Ok(estimate) => Some(estimate),
            Err(error) => {
                self.record_error(error);
                None
            }
        }
    }

    fn local_access_estimate(
        &self,
        table: &str,
        predicate: &uqa_execution::ScalarExpr,
    ) -> Option<uqa_planner::LocalAccessEstimate> {
        if predicate.contains_parameter() {
            return None;
        }
        match self.engine.try_table(table) {
            Ok(Some(_)) => {}
            Ok(None) => return None,
            Err(error) => {
                self.record_error(SQLError::Internal(format!(
                    "resolve optimizer storage table `{table}`: {error}"
                )));
                return None;
            }
        }
        match crate::operator_tree_bridge::estimate_local_access(self.engine, table, predicate, &[])
        {
            Ok(estimate) => estimate,
            Err(error) => {
                self.record_error(error);
                None
            }
        }
    }
}

#[cfg(test)]
pub(super) fn compile_logical_plans(
    engine: &Engine,
    sql: &str,
) -> Result<Vec<uqa_planner::UnifiedPlan>, SQLError> {
    if let Some(cached) = engine.cached_sql_statement(sql) {
        return Ok(vec![cached.logical_plan.as_ref().clone()]);
    }
    let statements = compile(sql)?;
    let plans = statements
        .iter()
        .cloned()
        .map(|statement| lower_statement(engine, statement))
        .collect::<Vec<_>>();
    if plans.len() == 1 {
        engine.cache_sql_statement(
            sql.to_string(),
            Arc::new(statements[0].clone()),
            Arc::new(plans[0].clone()),
        );
    }
    Ok(plans)
}

pub(super) fn lower_statement(engine: &Engine, statement: Statement) -> uqa_planner::UnifiedPlan {
    uqa_planner::UnifiedPlan::lower_with(statement, &|name: &str| {
        engine.has_registered_aggregate_function(name)
    })
}

pub(crate) fn optimize_engine_plan(
    engine: &Engine,
    plan: uqa_planner::UnifiedPlan,
) -> Result<uqa_planner::UnifiedPlan, SQLError> {
    let callback_error = std::cell::RefCell::new(None);
    let mut optimizer_config = uqa_planner::optimizer::OptimizerConfig::default();
    if volatility::unified_plan_contains_volatile_function(engine, &plan) {
        // Predicate prioritization and DPccp both move expressions across
        // physical evaluation boundaries.  A VOLATILE callback may observe
        // or mutate state on every call, so even a logically equivalent join
        // order can change SQL-visible behavior by changing its call count.
        optimizer_config.enable_filter_pushdown = false;
        optimizer_config.enable_join_reordering = false;
    }
    let statistics = EngineSourceStatistics {
        engine,
        error: &callback_error,
    };
    let optimized = uqa_planner::optimizer::optimize_with_aggregates_and_statistics(
        plan,
        &optimizer_config,
        &|name: &str| engine.has_registered_aggregate_function(name),
        &statistics,
    );
    if let Some(error) = callback_error.into_inner() {
        return Err(error);
    }
    optimized.map_err(|error| SQLError::Internal(format!("optimize SQL join order: {error}")))
}

/// Lower and execute an already-compiled statement through the same unified
/// plan entry point used by [`Engine::sql`]. SQL/PLpgSQL routine bodies call
/// this instead of retaining a private AST dispatcher.
pub(crate) fn execute_compiled_statement(
    engine: &Engine,
    statement: Statement,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    let plan = uqa_planner::UnifiedPlan::lower_with(statement, &|name: &str| {
        engine.has_registered_aggregate_function(name)
    });
    let plan = optimize_engine_plan(engine, plan)?;
    UnifiedPlanExecutor::new(engine, params).execute(&plan)
}
