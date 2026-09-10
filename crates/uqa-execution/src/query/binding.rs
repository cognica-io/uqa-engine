//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Adapt active engine statement inputs to SQL-owned static binding.

use super::CteScope;
use crate::{RowSchema, ScalarExpr};
use uqa_sql::plan::{
    CommandPlan, ExpressionPlan, ProjectionPlan, QueryBlockPlan, QueryPlan, SourcePlan, UnifiedPlan,
};
use uqa_sql::routines::RoutineResolution;
use uqa_sql::{ColumnType, SQLError, SQLParam};
mod context;
pub use context::binding_context;

pub fn analyze_prepared_command_schema<S: Clone>(
    routines: &dyn RoutineResolution,
    command: &CommandPlan,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<Option<RowSchema>, SQLError> {
    uqa_sql::binding::analyze_prepared_command_schema(
        routines,
        command,
        params,
        &binding_context(ctes)?,
    )
}

pub fn analyze_recursive_control_step<S: Clone>(
    routines: &dyn RoutineResolution,
    cte: &uqa_sql::plan::CtePlan,
    step: &QueryPlan,
    base_schema: RowSchema,
    params: &[SQLParam],
    ctes: &CteScope<S>,
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

pub fn infer_prepared_parameter_types<S: Clone>(
    routines: &dyn RoutineResolution,
    plan: &UnifiedPlan,
    declared: &[Option<ColumnType>],
    ctes: &CteScope<S>,
) -> Result<Vec<Option<ColumnType>>, SQLError> {
    uqa_sql::binding::infer_prepared_parameter_types(
        routines,
        plan,
        declared,
        &binding_context(ctes)?,
    )
}

pub fn bind_projection_output_schema<S: Clone>(
    routines: &dyn RoutineResolution,
    projections: &[ProjectionPlan],
    expression_schema: &RowSchema,
    star_schema: &RowSchema,
    subqueries: &[QueryPlan],
    params: &[SQLParam],
    ctes: &CteScope<S>,
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

pub fn analyze_projection_output_schema<S: Clone>(
    routines: &dyn RoutineResolution,
    projections: &[ProjectionPlan],
    expression_schema: &RowSchema,
    star_schema: &RowSchema,
    subqueries: &[QueryPlan],
    params: &[SQLParam],
    ctes: &CteScope<S>,
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

pub fn validate_query_block_expression_types<S: Clone>(
    routines: &dyn RoutineResolution,
    statement: &QueryBlockPlan,
    schema: &RowSchema,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<(), SQLError> {
    uqa_sql::binding::validate_query_block_expression_types(
        routines,
        statement,
        schema,
        params,
        &binding_context(ctes)?,
    )
}

pub fn validate_query_block_references<S: Clone>(
    routines: &dyn RoutineResolution,
    statement: &QueryBlockPlan,
    schema: &RowSchema,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<(), SQLError> {
    uqa_sql::binding::validate_query_block_references(
        routines,
        statement,
        schema,
        params,
        &binding_context(ctes)?,
    )
}

pub fn bind_query_plan_routines_for_storage<S: Clone>(
    engine: &dyn RoutineResolution,
    plan: &mut QueryPlan,
    params: &[SQLParam],
    ctes: &CteScope<S>,
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

pub fn bind_expression_plan_routines_for_storage<S: Clone>(
    engine: &dyn RoutineResolution,
    plan: &mut ExpressionPlan,
    params: &[SQLParam],
    ctes: &CteScope<S>,
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

pub fn bind_query_plan_schema<S: Clone>(
    routines: &dyn RoutineResolution,
    plan: &QueryPlan,
    params: &[SQLParam],
    ctes: &CteScope<S>,
    outer: Option<&RowSchema>,
) -> Result<RowSchema, SQLError> {
    uqa_sql::binding::bind_query_plan_schema(routines, plan, params, &binding_context(ctes)?, outer)
}

pub fn bind_expression_plan_type<S: Clone>(
    routines: &dyn RoutineResolution,
    plan: &ExpressionPlan,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<Option<ColumnType>, SQLError> {
    uqa_sql::binding::bind_expression_plan_type(routines, plan, params, &binding_context(ctes)?)
}

pub fn analyze_expression_plan_type<S: Clone>(
    routines: &dyn RoutineResolution,
    plan: &ExpressionPlan,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<Option<ColumnType>, SQLError> {
    uqa_sql::binding::analyze_expression_plan_type(routines, plan, params, &binding_context(ctes)?)
}

pub fn analyze_query_plan_schema<S: Clone>(
    routines: &dyn RoutineResolution,
    plan: &QueryPlan,
    params: &[SQLParam],
    ctes: &CteScope<S>,
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

pub fn values_types_in_scope<S: Clone>(
    routines: &dyn RoutineResolution,
    rows: &[Vec<ScalarExpr>],
    subqueries: &[QueryPlan],
    schema: Option<&RowSchema>,
    params: &[SQLParam],
    ctes: &CteScope<S>,
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

pub fn bind_source_plan_schema<S: Clone>(
    routines: &dyn RoutineResolution,
    source: &SourcePlan,
    params: &[SQLParam],
    ctes: &CteScope<S>,
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

pub fn analyze_source_plan_schema<S: Clone>(
    routines: &dyn RoutineResolution,
    source: &SourcePlan,
    params: &[SQLParam],
    ctes: &CteScope<S>,
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

pub fn bind_source_plan_schema_for_execution<S: Clone>(
    routines: &dyn RoutineResolution,
    source: &mut SourcePlan,
    params: &[SQLParam],
    ctes: &CteScope<S>,
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

pub use uqa_sql::binding::{
    extend_cte_generated_schema, overlay_outer_schema, with_query_table_pseudo_columns,
};

impl<S: Clone> uqa_sql::semantics::returning::ReturningScope for CteScope<S> {
    fn binding_snapshot(&self) -> Result<uqa_sql::binding::snapshot::BindingSnapshot, SQLError> {
        binding_context(self).map(uqa_sql::binding::snapshot::BindingSnapshot::from)
    }
}
