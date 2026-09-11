//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture statement metadata for planner-owned predicate placement.

use crate::{capabilities::query_scope::CteScope, Engine};
use std::collections::BTreeMap;
use uqa_planner::filter_pushdown::context::{FilterPushdownContext, FilterPushdownScope};
use uqa_sql::{
    plan::{
        source_projection::{ColumnPrune, QualifierFilters},
        QueryBlockPlan, QueryPlan, SourcePlan,
    },
    SQLError, ScalarExpr,
};

fn with_context<T>(engine: &Engine, action: impl FnOnce(FilterPushdownContext<'_>) -> T) -> T {
    let catalog = engine.catalog_read_view();
    let resolution = engine.session_execution_view().relation_name_resolution();
    action(FilterPushdownContext {
        volatility: engine,
        correlation: uqa_sql::binding::correlation::CorrelationContext {
            catalog: &catalog,
            resolution: &resolution,
        },
        optimizer: &|plan| {
            crate::capabilities::statement_planning::optimize_engine_plan(engine, plan)
        },
    })
}
fn with_scope<T>(
    ctes: &CteScope,
    action: impl FnOnce(FilterPushdownScope<'_>) -> Result<T, SQLError>,
) -> Result<T, SQLError> {
    let catalog = ctes.catalog_read_view()?;
    let resolution = ctes.relation_name_resolution()?;
    action(FilterPushdownScope {
        catalog: &catalog,
        resolution: &resolution,
        is_visible_cte: &|name| ctes.is_visible_cte(name),
    })
}

pub(crate) fn qualifier_filters_for_stmt(
    engine: &Engine,
    stmt: &QueryBlockPlan,
    from: &SourcePlan,
    ctes: &CteScope,
) -> Result<Option<QualifierFilters>, SQLError> {
    with_context(engine, |context| {
        with_scope(ctes, |scope| {
            uqa_planner::filter_pushdown::qualifier_filters_for_stmt(context, stmt, from, scope)
        })
    })
}

pub(crate) fn final_filter_after_qualifier_pushdown(
    engine: &Engine,
    stmt: &QueryBlockPlan,
    from: &SourcePlan,
    filters: Option<&QualifierFilters>,
    ctes: &CteScope,
) -> Result<Option<ScalarExpr>, SQLError> {
    with_context(engine, |context| {
        with_scope(ctes, |scope| {
            uqa_planner::filter_pushdown::final_filter_after_qualifier_pushdown(
                context, stmt, from, filters, scope,
            )
        })
    })
}

pub(crate) fn cte_output_filters(
    engine: &Engine,
    plan: &QueryPlan,
    ctes: &CteScope,
) -> Result<BTreeMap<String, (String, ScalarExpr)>, SQLError> {
    with_context(engine, |context| {
        with_scope(ctes, |scope| {
            uqa_planner::filter_pushdown::cte_output_filters(context, plan, scope)
        })
    })
}

pub(crate) fn push_output_filter_into_query_plan(
    engine: &Engine,
    plan: &QueryPlan,
    qualifier: &str,
    filter: &ScalarExpr,
    output_columns_override: Option<&[String]>,
) -> Result<Option<QueryPlan>, SQLError> {
    with_context(engine, |context| {
        uqa_planner::filter_pushdown::push_output_filter_into_query_plan(
            context,
            plan,
            qualifier,
            filter,
            output_columns_override,
        )
    })
}

pub(crate) fn column_prune_for_stmt(
    engine: &Engine,
    stmt: &QueryBlockPlan,
    from: &SourcePlan,
    ctes: &CteScope,
) -> Result<Option<ColumnPrune>, SQLError> {
    column_prune_for_stmt_with_filter(engine, stmt, from, stmt.r#where.as_ref(), ctes)
}

pub(crate) fn column_prune_for_stmt_with_filter(
    engine: &Engine,
    stmt: &QueryBlockPlan,
    from: &SourcePlan,
    filter: Option<&ScalarExpr>,
    ctes: &CteScope,
) -> Result<Option<ColumnPrune>, SQLError> {
    let catalog = ctes.catalog_read_view()?;
    let resolution = ctes.relation_name_resolution()?;
    uqa_planner::column_pruning::column_prune_for_stmt_with_filter(
        uqa_planner::column_pruning::ColumnPruneContext {
            catalog: &catalog,
            resolution: &resolution,
            volatility: engine,
            is_visible_cte: &|name| ctes.is_visible_cte(name),
        },
        stmt,
        from,
        filter,
    )
}
