//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture statement metadata for planner-owned predicate placement.

use crate::{capabilities::query_scope::CteScope, Engine};
use uqa_planner::filter_pushdown::context::{FilterPushdownContext, FilterPushdownScope};
use uqa_sql::{
    plan::{
        source_projection::{ColumnPrune, QualifierFilters},
        QueryBlockPlan, QueryPlan, SourcePlan,
    },
    SQLError, ScalarExpr,
};

fn with_context<T>(
    engine: &Engine,
    action: impl FnOnce(
        FilterPushdownContext<'_>,
        &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<T, SQLError>,
) -> Result<T, SQLError> {
    let resources =
        uqa_execution::query::relational::QueryExpressionFactory::production_resources(engine)?;
    let control = resources.control();
    let catalog = engine.catalog_read_view();
    let resolution =
        uqa_execution::catalog::services::CatalogSession::relation_name_resolution_with_control(
            engine, &control,
        )?;
    action(
        FilterPushdownContext {
            volatility: engine,
            correlation: uqa_sql::binding::correlation::CorrelationContext {
                routines: engine,
                catalog: &catalog,
                resolution: &resolution,
                scoped_relations: None,
            },
            optimizer: &|plan, control| {
                uqa_planner::statement_planning::executable::optimize_retained_query(
                    &engine.statement_planning_context(),
                    plan,
                    control,
                )
            },
        },
        &control,
    )
}
fn with_scope<T>(
    ctes: &CteScope,
    control: &uqa_core::memory::ProductionControl<'_>,
    action: impl FnOnce(FilterPushdownScope<'_>) -> Result<T, SQLError>,
) -> Result<T, SQLError> {
    let catalog = ctes.catalog_read_view()?;
    let resolution = ctes.relation_name_resolution_with_control(control)?;
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
    with_context(engine, |context, control| {
        with_scope(ctes, control, |scope| {
            uqa_planner::filter_pushdown::qualifier_filters_for_stmt_with_control(
                context, stmt, from, scope, control,
            )
        })
    })
}

pub(crate) fn final_filter_after_qualifier_pushdown(
    engine: &Engine,
    stmt: &QueryBlockPlan,
    from: &SourcePlan,
    filters: Option<&QualifierFilters>,
    ctes: &CteScope,
) -> Result<Option<uqa_sql::plan::OwnedScalarExpr>, SQLError> {
    with_context(engine, |context, control| {
        with_scope(ctes, control, |scope| {
            uqa_planner::filter_pushdown::final_filter_after_qualifier_pushdown_with_control(
                context, stmt, from, filters, scope, control,
            )
        })
    })
}

pub(crate) fn cte_output_filters(
    engine: &Engine,
    plan: &QueryPlan,
    ctes: &CteScope,
) -> Result<uqa_sql::plan::source_projection::CteOutputFilters, SQLError> {
    with_context(engine, |context, control| {
        with_scope(ctes, control, |scope| {
            uqa_planner::filter_pushdown::cte_output_filters_with_control(
                context, plan, scope, control,
            )
        })
    })
}

pub(crate) fn push_output_filter_into_query_plan(
    engine: &Engine,
    plan: &QueryPlan,
    qualifier: &str,
    filter: &ScalarExpr,
    output_columns_override: Option<&[String]>,
) -> Result<Option<uqa_sql::plan::OwnedQueryPlan>, SQLError> {
    with_context(engine, |context, control| {
        uqa_planner::filter_pushdown::push_output_filter_into_query_plan_with_control(
            context,
            plan,
            qualifier,
            filter,
            output_columns_override,
            control,
        )
    })
}

pub(crate) fn column_prune_for_stmt(
    engine: &Engine,
    stmt: &QueryBlockPlan,
    from: &SourcePlan,
    ctes: &CteScope,
    schema: Option<&uqa_sql::RowSchema>,
) -> Result<Option<ColumnPrune>, SQLError> {
    column_prune_for_stmt_with_filter(engine, stmt, from, stmt.r#where.as_ref(), ctes, schema)
}

pub(crate) fn push_derived_output_filter(
    engine: &Engine,
    plan: &QueryPlan,
    qualifier: &str,
    filter: &ScalarExpr,
    output_columns: Option<&[String]>,
) -> Result<Option<uqa_sql::plan::OwnedQueryPlan>, SQLError> {
    with_context(engine, |context, control| {
        uqa_planner::filter_pushdown::push_derived_output_filter_with_control(
            context,
            plan,
            qualifier,
            filter,
            output_columns,
            control,
        )
    })
}

pub(crate) fn column_prune_for_stmt_with_filter(
    engine: &Engine,
    stmt: &QueryBlockPlan,
    from: &SourcePlan,
    filter: Option<&ScalarExpr>,
    ctes: &CteScope,
    schema: Option<&uqa_sql::RowSchema>,
) -> Result<Option<ColumnPrune>, SQLError> {
    let catalog = ctes.catalog_read_view()?;
    let resolution = ctes.relation_name_resolution()?;
    uqa_planner::column_pruning::column_prune_for_stmt_with_filter(
        uqa_planner::column_pruning::ColumnPruneContext {
            catalog: &catalog,
            resolution: &resolution,
            volatility: engine,
            is_visible_cte: &|name| ctes.is_visible_cte(name),
            source_schema: schema,
        },
        stmt,
        from,
        filter,
    )
}
