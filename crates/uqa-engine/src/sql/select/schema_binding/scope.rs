//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Outer-scope composition and common row-value typing.

use uqa_execution::RowSchema;
use uqa_planner::{ExpressionPlan, QueryPlan};
use uqa_sql::ast::ColumnType;
use uqa_sql::{SQLError, SQLParam};

use super::{CteScope, ScalarExpr, SchemaScope};
use crate::engine_user_functions::RoutineResolution;

/// Derive the exact output row type of a query plan without executing it.
pub(in crate::sql) fn bind_query_plan_schema(
    routines: &dyn RoutineResolution,
    plan: &QueryPlan,
    params: &[SQLParam],
    ctes: &CteScope,
    outer: Option<&RowSchema>,
) -> Result<RowSchema, SQLError> {
    SchemaScope::from_execution_scope(ctes)?.bind_query(routines, plan, params, outer)
}

/// Derive the declared SQL type of a standalone expression plan without executing it. The plan-owned subquery arena participates in type resolution so scalar subqueries retain their projected type at command boundaries such as `CALL`.
pub(in crate::sql) fn bind_expression_plan_type(
    routines: &dyn RoutineResolution,
    plan: &ExpressionPlan,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<Option<ColumnType>, SQLError> {
    SchemaScope::from_execution_scope(ctes)?.bind_expression_type(
        routines,
        &plan.scalar,
        &RowSchema::default(),
        &plan.subqueries,
        params,
        None,
    )
}

/// Analyze every catalog and scalar reference and derive the exact output row type without executing the query.
pub(in crate::sql) fn analyze_query_plan_schema(
    routines: &dyn RoutineResolution,
    plan: &QueryPlan,
    params: &[SQLParam],
    ctes: &CteScope,
    outer: Option<&RowSchema>,
) -> Result<RowSchema, SQLError> {
    SchemaScope::for_analysis(ctes)?.bind_query(routines, plan, params, outer)
}

pub(in crate::sql) fn analyze_query_plan_schema_with_catalog(
    routines: &dyn RoutineResolution,
    plan: &QueryPlan,
    params: &[SQLParam],
    catalog: crate::engine_capabilities::CatalogReadView,
    resolution: crate::engine_capabilities::RelationNameResolution,
) -> Result<RowSchema, SQLError> {
    SchemaScope::for_catalog_analysis(catalog, resolution).bind_query(routines, plan, params, None)
}

pub(in crate::sql) fn overlay_outer_schema(
    current: &RowSchema,
    outer: Option<&RowSchema>,
) -> RowSchema {
    outer.map_or_else(
        || current.clone(),
        |outer| RowSchema::with_outer_schema(current, outer),
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
    SchemaScope::from_execution_scope(ctes)?
        .bind_values_types(routines, rows, subqueries, schema, params, schema)
}

pub(super) fn merge_types(
    left: Option<&ColumnType>,
    right: Option<&ColumnType>,
) -> Result<Option<ColumnType>, SQLError> {
    match (left, right) {
        (None, None) => Ok(None),
        (Some(ty), None) | (None, Some(ty)) => Ok(Some(ty.clone())),
        (Some(left), Some(right)) => uqa_execution::common_type(left, right).map(Some),
    }
}
