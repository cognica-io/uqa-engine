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
pub(crate) mod sources;
pub use context::binding_context;
mod subqueries;
pub use subqueries::resolve_scalar_subquery_type;

pub(crate) fn source_depends_on_current_row<S: Clone>(
    routines: &dyn RoutineResolution,
    source: &SourcePlan,
    input: &RowSchema,
    ctes: &CteScope<S>,
) -> Result<bool, SQLError> {
    let catalog = ctes.catalog_read_view()?;
    let resolution = ctes.relation_name_resolution()?;
    uqa_sql::binding::correlation::source_depends_on_current_row(
        uqa_sql::binding::correlation::CorrelationContext {
            routines,
            catalog: &catalog,
            resolution: &resolution,
            scoped_relations: Some(ctes),
        },
        source,
        &ctes.scalar_subqueries,
        input,
    )
}

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
    outer: Option<&RowSchema>,
) -> Result<(), SQLError> {
    uqa_sql::binding::validate_query_block_references(
        routines,
        statement,
        schema,
        params,
        &binding_context(ctes)?,
        outer,
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

/// Record each call's input collation in a stored query whose routines are bound.
pub fn assign_query_plan_input_collations<S: Clone>(
    engine: &dyn RoutineResolution,
    plan: &mut QueryPlan,
    ctes: &CteScope<S>,
) -> Result<(), SQLError> {
    uqa_sql::binding::assign_stored_query_input_collations(engine, plan, &binding_context(ctes)?)
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

pub(crate) fn restore_query_layouts<S: Clone>(
    routines: &dyn RoutineResolution,
    plan: &mut QueryPlan,
    params: &[SQLParam],
    ctes: &CteScope<S>,
    outer: Option<&RowSchema>,
) -> Result<(), SQLError> {
    uqa_sql::binding::restore_query_layouts(routines, plan, params, &binding_context(ctes)?, outer)
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
    values_schema_in_scope(routines, rows, subqueries, schema, params, ctes)
        .map(|schema| schema.column_types().to_vec())
}

pub fn values_schema_in_scope<S: Clone>(
    routines: &dyn RoutineResolution,
    rows: &[Vec<ScalarExpr>],
    subqueries: &[QueryPlan],
    schema: Option<&RowSchema>,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<RowSchema, SQLError> {
    uqa_sql::binding::values_schema_in_scope(
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

pub fn bind_query_block_for_execution<'a, S: Clone>(
    routines: &dyn RoutineResolution,
    block: &'a QueryBlockPlan,
    params: &[SQLParam],
    ctes: &CteScope<S>,
    outer: Option<&RowSchema>,
    defer_distinct_limit: bool,
) -> Result<(std::borrow::Cow<'a, QueryBlockPlan>, RowSchema), SQLError> {
    uqa_sql::binding::bind_query_block_for_execution(
        routines,
        block,
        params,
        &binding_context(ctes)?,
        outer,
        defer_distinct_limit,
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

pub fn analyze_command_parameters<S: Clone>(
    routines: &dyn RoutineResolution,
    command: &CommandPlan,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<(), SQLError> {
    uqa_sql::binding::statements::analyze_command_parameters(routines, command, params, ctes)
}

impl<S: Clone> uqa_sql::binding::statements::StatementBindingScope for CteScope<S> {
    fn binding_context(&self) -> Result<uqa_sql::binding::BindingContext<'_>, SQLError> {
        binding_context(self)
    }
}

/// Add SQL-owned structural identities while retaining the outer row's existing physical fragments.
pub(crate) fn bind_outer_rewrite_row<'a>(
    block: &QueryBlockPlan,
    outer: &'a crate::OwnedPhysicalRow,
) -> std::borrow::Cow<'a, crate::OwnedPhysicalRow> {
    match uqa_sql::semantics::view_rewrite::bind_outer_rewrite_scope(block, &outer.schema) {
        std::borrow::Cow::Borrowed(_) => std::borrow::Cow::Borrowed(outer),
        std::borrow::Cow::Owned(schema) => std::borrow::Cow::Owned(crate::OwnedPhysicalRow {
            schema,
            row: outer.row.clone(),
        }),
    }
}

/// Source syntax and the scalar arena selected with its actual preceding row descriptor.
pub(crate) struct OuterSourceBinding {
    pub(crate) source: Box<SourcePlan>,
    pub(crate) subqueries: Vec<QueryPlan>,
}

pub(crate) fn bind_outer_source_rewrites<S: Clone>(
    routines: &dyn uqa_sql::routines::RoutineResolution,
    source: &SourcePlan,
    params: &[SQLParam],
    ctes: &CteScope<S>,
    scope: uqa_sql::binding::SourceScope<'_>,
) -> Result<Option<OuterSourceBinding>, SQLError> {
    let Some(rewrites) = &ctes.source_outer_rewrites else {
        return Ok(None);
    };
    let context = binding_context(ctes)?;
    let mut subqueries = ctes.scalar_subqueries.to_vec();
    let source = uqa_sql::semantics::view_rewrite::resolve_outer_source_rewrites(
        source,
        rewrites,
        &mut subqueries,
        scope,
        &mut |source, subqueries, scope| {
            uqa_sql::binding::bind_source_input_schema(
                routines,
                source,
                params,
                &uqa_sql::binding::BindingContext {
                    scalar_subqueries: subqueries,
                    ..context.clone()
                },
                scope,
            )
        },
    )?
    .into_owned();
    Ok(Some(OuterSourceBinding {
        source: Box::new(source),
        subqueries,
    }))
}
