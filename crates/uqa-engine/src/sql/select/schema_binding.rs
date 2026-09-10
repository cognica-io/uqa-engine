//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Adapt active engine statement inputs to SQL-owned static binding.

use super::CteScope;
use crate::engine_user_functions::RoutineResolution;
use uqa_execution::{RowSchema, ScalarExpr};
use uqa_planner::{
    CommandPlan, ExpressionPlan, ProjectionPlan, QueryBlockPlan, QueryPlan, SourcePlan, UnifiedPlan,
};
use uqa_sql::{ColumnType, SQLError, SQLParam};
mod context;
use context::binding_context;

pub(in crate::sql) fn analyze_prepared_command_schema(
    routines: &dyn RoutineResolution,
    command: &CommandPlan,
    params: &[SQLParam],
    ctes: &super::CteScope,
) -> Result<Option<RowSchema>, SQLError> {
    uqa_sql::binding::analyze_prepared_command_schema(
        routines,
        command,
        params,
        &binding_context(ctes)?,
    )
}

pub(in crate::sql) fn analyze_recursive_control_step(
    routines: &dyn RoutineResolution,
    cte: &uqa_planner::CtePlan,
    step: &QueryPlan,
    base_schema: RowSchema,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<(), SQLError> {
    uqa_sql::binding::analyze_recursive_control_step(
        routines,
        cte,
        step,
        base_schema,
        params,
        &binding_context(ctes)?,
    )
}

pub(in crate::sql) fn infer_prepared_parameter_types(
    routines: &dyn RoutineResolution,
    plan: &UnifiedPlan,
    declared: &[Option<ColumnType>],
    ctes: &CteScope,
) -> Result<Vec<Option<ColumnType>>, SQLError> {
    uqa_sql::binding::infer_prepared_parameter_types(
        routines,
        plan,
        declared,
        &binding_context(ctes)?,
    )
}

pub(in crate::sql) fn bind_projection_output_schema(
    routines: &dyn RoutineResolution,
    projections: &[ProjectionPlan],
    expression_schema: &RowSchema,
    star_schema: &RowSchema,
    subqueries: &[QueryPlan],
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<RowSchema, SQLError> {
    uqa_sql::binding::bind_projection_output_schema(
        routines,
        projections,
        expression_schema,
        star_schema,
        subqueries,
        params,
        &binding_context(ctes)?,
    )
}

pub(in crate::sql) fn analyze_projection_output_schema(
    routines: &dyn RoutineResolution,
    projections: &[ProjectionPlan],
    expression_schema: &RowSchema,
    star_schema: &RowSchema,
    subqueries: &[QueryPlan],
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<RowSchema, SQLError> {
    uqa_sql::binding::analyze_projection_output_schema(
        routines,
        projections,
        expression_schema,
        star_schema,
        subqueries,
        params,
        &binding_context(ctes)?,
    )
}

pub(in crate::sql) fn validate_query_block_expression_types(
    routines: &dyn RoutineResolution,
    statement: &QueryBlockPlan,
    schema: &RowSchema,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<(), SQLError> {
    uqa_sql::binding::validate_query_block_expression_types(
        routines,
        statement,
        schema,
        params,
        &binding_context(ctes)?,
    )
}

pub(in crate::sql) fn validate_query_block_references(
    routines: &dyn RoutineResolution,
    statement: &QueryBlockPlan,
    schema: &RowSchema,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<(), SQLError> {
    uqa_sql::binding::validate_query_block_references(
        routines,
        statement,
        schema,
        params,
        &binding_context(ctes)?,
    )
}

pub(in crate::sql) fn bind_query_plan_routines_for_storage(
    engine: &dyn RoutineResolution,
    plan: &mut QueryPlan,
    params: &[SQLParam],
    ctes: &CteScope,
    outer: Option<&RowSchema>,
) -> Result<RowSchema, SQLError> {
    uqa_sql::binding::bind_query_plan_routines_for_storage(
        engine,
        plan,
        params,
        &binding_context(ctes)?,
        outer,
    )
}

pub(in crate::sql) fn bind_expression_plan_routines_for_storage(
    engine: &dyn RoutineResolution,
    plan: &mut ExpressionPlan,
    params: &[SQLParam],
    ctes: &CteScope,
    schema: &RowSchema,
) -> Result<Option<ColumnType>, SQLError> {
    uqa_sql::binding::bind_expression_plan_routines_for_storage(
        engine,
        plan,
        params,
        &binding_context(ctes)?,
        schema,
    )
}

pub(in crate::sql) fn bind_query_plan_schema(
    routines: &dyn RoutineResolution,
    plan: &QueryPlan,
    params: &[SQLParam],
    ctes: &CteScope,
    outer: Option<&RowSchema>,
) -> Result<RowSchema, SQLError> {
    uqa_sql::binding::bind_query_plan_schema(routines, plan, params, &binding_context(ctes)?, outer)
}

pub(in crate::sql) fn bind_expression_plan_type(
    routines: &dyn RoutineResolution,
    plan: &ExpressionPlan,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<Option<ColumnType>, SQLError> {
    uqa_sql::binding::bind_expression_plan_type(routines, plan, params, &binding_context(ctes)?)
}

pub(in crate::sql) fn analyze_expression_plan_type(
    routines: &dyn RoutineResolution,
    plan: &ExpressionPlan,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<Option<ColumnType>, SQLError> {
    uqa_sql::binding::analyze_expression_plan_type(routines, plan, params, &binding_context(ctes)?)
}

pub(in crate::sql) fn analyze_query_plan_schema(
    routines: &dyn RoutineResolution,
    plan: &QueryPlan,
    params: &[SQLParam],
    ctes: &CteScope,
    outer: Option<&RowSchema>,
) -> Result<RowSchema, SQLError> {
    uqa_sql::binding::analyze_query_plan_schema(
        routines,
        plan,
        params,
        &binding_context(ctes)?,
        outer,
    )
}

pub(in crate::sql) fn analyze_query_plan_schema_with_catalog(
    routines: &dyn RoutineResolution,
    plan: &QueryPlan,
    params: &[SQLParam],
    catalog: crate::engine_capabilities::CatalogReadView,
    resolution: crate::engine_capabilities::RelationNameResolution,
) -> Result<RowSchema, SQLError> {
    uqa_sql::binding::analyze_query_plan_schema_with_catalog(
        routines,
        plan,
        params,
        std::sync::Arc::new(catalog),
        resolution,
    )
}

pub(in crate::sql) fn values_types_in_scope(
    routines: &dyn RoutineResolution,
    rows: &[Vec<ScalarExpr>],
    subqueries: &[QueryPlan],
    schema: Option<&RowSchema>,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<Vec<Option<ColumnType>>, SQLError> {
    uqa_sql::binding::values_types_in_scope(
        routines,
        rows,
        subqueries,
        schema,
        params,
        &binding_context(ctes)?,
    )
}

pub(in crate::sql) fn bind_source_plan_schema(
    routines: &dyn RoutineResolution,
    source: &SourcePlan,
    params: &[SQLParam],
    ctes: &CteScope,
    outer: Option<&RowSchema>,
) -> Result<RowSchema, SQLError> {
    uqa_sql::binding::bind_source_plan_schema(
        routines,
        source,
        params,
        &binding_context(ctes)?,
        outer,
    )
}

pub(in crate::sql) fn analyze_source_plan_schema(
    routines: &dyn RoutineResolution,
    source: &SourcePlan,
    params: &[SQLParam],
    ctes: &CteScope,
    outer: Option<&RowSchema>,
) -> Result<RowSchema, SQLError> {
    uqa_sql::binding::analyze_source_plan_schema(
        routines,
        source,
        params,
        &binding_context(ctes)?,
        outer,
    )
}

pub(in crate::sql) fn bind_source_plan_schema_for_execution(
    routines: &dyn RoutineResolution,
    source: &mut SourcePlan,
    params: &[SQLParam],
    ctes: &CteScope,
    outer: Option<&RowSchema>,
) -> Result<RowSchema, SQLError> {
    uqa_sql::binding::bind_source_plan_schema_for_execution(
        routines,
        source,
        params,
        &binding_context(ctes)?,
        outer,
    )
}

pub(in crate::sql) use uqa_sql::binding::{
    extend_cte_generated_schema, overlay_outer_schema, with_query_table_pseudo_columns,
};
