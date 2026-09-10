//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{volatility, Engine, SQLError, SQLParam, SQLResult, Statement, UnifiedPlanExecutor};

mod plan_cost;
mod rule_inputs;
pub(crate) use plan_cost::estimate_engine_plan;

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
                hierarchy_row_count(self.engine, table),
                self.engine.try_query_column_stats(table),
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
        let uqa_planner::SourcePlan::Function {
            name,
            relations,
            args,
            ..
        } = source
        else {
            return None;
        };
        if args.iter().any(|argument| {
            argument.contains_parameter()
                || volatility::expr_contains_volatile_function(self.engine, argument)
        }) {
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
            relations.as_ref(),
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
        if volatility::expr_contains_volatile_function(self.engine, predicate) {
            return None;
        }
        if predicate.contains_parameter() {
            return match plan_cost::parameterized_access(self, table, predicate) {
                Ok(estimate) => estimate,
                Err(error) => {
                    self.record_error(error);
                    None
                }
            };
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

fn hierarchy_row_count(engine: &Engine, table: &str) -> Result<u64, SQLError> {
    let mut total = 0_u64;
    for member in engine.query_hierarchy_scan_tables(table, true)? {
        total = total
            .checked_add(engine.table_doc_count(&member)?)
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "optimizer hierarchy row count overflow for `{table}`"
                ))
            })?;
    }
    Ok(total)
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

/// Analyze executable statements before optimizer evaluation can raise SQL errors.
pub(crate) fn plan_for_execution(
    engine: &Engine,
    plan: uqa_planner::UnifiedPlan,
    params: &[SQLParam],
) -> Result<uqa_planner::UnifiedPlan, SQLError> {
    analyze_executable_plan(engine, &plan, params)?;
    optimize_engine_plan(engine, plan)
}

fn analyze_executable_plan(
    engine: &Engine,
    plan: &uqa_planner::UnifiedPlan,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    use uqa_planner::{CommandPlan, UnifiedPlan};
    let scope = super::select::CteScope::new_for_current_routine(engine);
    match plan {
        UnifiedPlan::Query(query) => {
            super::select::analyze_query_plan_schema(engine, query, params, &scope, None)?;
        }
        UnifiedPlan::Command(command) => match command.as_ref() {
            CommandPlan::Explain { body, .. } => analyze_executable_plan(engine, body, params)?,
            CommandPlan::CreateTableAs { query, .. }
            | CommandPlan::CreateMaterializedView { query, .. }
            | CommandPlan::DeclareCursor { query, .. } => {
                super::select::analyze_query_plan_schema(engine, query, params, &scope, None)?;
            }
            _ => {
                if command.mutation_target().is_some() {
                    super::prepared::analyze_command_parameters(engine, command, params, &scope)?;
                }
                super::select::analyze_prepared_command_schema(engine, command, params, &scope)?;
            }
        },
    }
    Ok(())
}

pub(crate) fn optimize_engine_query(
    engine: &Engine,
    query: &uqa_planner::QueryPlan,
) -> Result<uqa_planner::QueryPlan, SQLError> {
    match optimize_engine_plan(
        engine,
        uqa_planner::UnifiedPlan::Query(Box::new(query.clone())),
    )? {
        uqa_planner::UnifiedPlan::Query(query) => Ok(*query),
        uqa_planner::UnifiedPlan::Command(_) => Err(SQLError::Internal(
            "query optimization produced a command".into(),
        )),
    }
}

pub(crate) fn optimize_engine_plan(
    engine: &Engine,
    mut plan: uqa_planner::UnifiedPlan,
) -> Result<uqa_planner::UnifiedPlan, SQLError> {
    rule_inputs::rewrite_plan(engine, &mut plan)?;
    let callback_error = std::cell::RefCell::new(None);
    let statistics = EngineSourceStatistics {
        engine,
        error: &callback_error,
    };
    let optimized = optimize_plan_with_statistics(engine, plan, &statistics);
    if let Some(error) = callback_error.into_inner() {
        return Err(error);
    }
    optimized
}

fn optimize_plan_with_statistics(
    engine: &Engine,
    plan: uqa_planner::UnifiedPlan,
    statistics: &dyn uqa_planner::SourceStatistics,
) -> Result<uqa_planner::UnifiedPlan, SQLError> {
    let mut optimizer_config = uqa_planner::optimizer::OptimizerConfig::default();
    if volatility::unified_plan_contains_volatile_function(engine, &plan) {
        // Predicate prioritization and DPccp both move expressions across
        // physical evaluation boundaries.  A VOLATILE callback may observe
        // or mutate state on every call, so even a logically equivalent join
        // order can change SQL-visible behavior by changing its call count.
        optimizer_config.enable_filter_pushdown = false;
        optimizer_config.enable_join_reordering = false;
    }
    let optimized = uqa_planner::optimizer::optimize_with_aggregates_and_statistics(
        plan,
        &optimizer_config,
        &|name: &str| engine.has_registered_aggregate_function(name),
        statistics,
    );
    optimized.map_err(|error| match error {
        uqa_planner::optimizer::OptimizerError::Expression(error) => error,
        uqa_planner::optimizer::OptimizerError::JoinGraph(error) => {
            SQLError::Internal(format!("optimize SQL join order: {error}"))
        }
    })
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
    let plan = plan_for_execution(engine, plan, params)?;
    UnifiedPlanExecutor::new_nested(engine, params).execute(&plan)
}

pub(crate) fn execute_compiled_statement_with_privilege_subject(
    engine: &Engine,
    statement: Statement,
    params: &[SQLParam],
    privilege_subject: &str,
) -> Result<SQLResult, SQLError> {
    let mut plan = uqa_planner::UnifiedPlan::lower_with(statement, &|name: &str| {
        engine.has_registered_aggregate_function(name)
    });
    super::catalog_statement_routines::mark_catalog_statement_relations_bound(&mut plan)?;
    let plan = plan_for_execution(engine, plan, params)?;
    UnifiedPlanExecutor::new_nested(engine, params)
        .with_privilege_subject(privilege_subject)
        .execute(&plan)
}
