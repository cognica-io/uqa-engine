//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Outer-scope composition and common row-value typing.

use crate::ast::ColumnType;
use crate::plan::{ExpressionPlan, QueryPlan};
use crate::RowSchema;
use crate::{SQLError, SQLParam};

use super::{BindingContext, ScalarExpr, SchemaScope};
use crate::routines::RoutineResolution;

/// Derive the exact output row type of a query plan without executing it.
pub fn bind_query_plan_schema(
    routines: &dyn RoutineResolution,
    plan: &QueryPlan,
    params: &[SQLParam],
    ctes: &BindingContext,
    outer: Option<&RowSchema>,
) -> Result<RowSchema, SQLError> {
    SchemaScope::from_context(ctes)?.bind_query(routines, plan, params, outer)
}

/// Derive the declared SQL type of a standalone expression plan without executing it. The plan-owned subquery arena participates in type resolution so scalar subqueries retain their projected type at command boundaries such as `CALL`.
pub fn bind_expression_plan_type(
    routines: &dyn RoutineResolution,
    plan: &ExpressionPlan,
    params: &[SQLParam],
    ctes: &BindingContext,
) -> Result<Option<ColumnType>, SQLError> {
    SchemaScope::from_context(ctes)?.bind_expression_type(
        routines,
        &plan.scalar,
        &RowSchema::default(),
        &plan.subqueries,
        params,
        None,
    )
}

/// Validate a command argument's names and types without running its expressions.
pub fn analyze_expression_plan_type(
    routines: &dyn RoutineResolution,
    plan: &ExpressionPlan,
    params: &[SQLParam],
    ctes: &BindingContext,
) -> Result<Option<ColumnType>, SQLError> {
    SchemaScope::for_analysis(ctes)?.bind_expression_type(
        routines,
        &plan.scalar,
        &RowSchema::default(),
        &plan.subqueries,
        params,
        None,
    )
}

/// Analyze every catalog and scalar reference and derive the exact output row type without executing the query.
pub fn analyze_query_plan_schema(
    routines: &dyn RoutineResolution,
    plan: &QueryPlan,
    params: &[SQLParam],
    ctes: &BindingContext,
    outer: Option<&RowSchema>,
) -> Result<RowSchema, SQLError> {
    SchemaScope::for_analysis(ctes)?.bind_query(routines, plan, params, outer)
}

pub fn analyze_query_plan_schema_with_catalog(
    routines: &dyn RoutineResolution,
    plan: &QueryPlan,
    params: &[SQLParam],
    catalog: crate::catalog::analysis::CatalogReadView,
    resolution: crate::catalog::resolution::RelationNameResolution,
) -> Result<RowSchema, SQLError> {
    SchemaScope::for_catalog_analysis(catalog, resolution).bind_query(routines, plan, params, None)
}

pub fn overlay_outer_schema(current: &RowSchema, outer: Option<&RowSchema>) -> RowSchema {
    outer.map_or_else(
        || current.clone(),
        |outer| RowSchema::with_outer_schema(current, outer),
    )
}

pub fn values_types_in_scope(
    routines: &dyn RoutineResolution,
    rows: &[Vec<ScalarExpr>],
    subqueries: &[QueryPlan],
    schema: Option<&RowSchema>,
    params: &[SQLParam],
    ctes: &BindingContext,
) -> Result<Vec<Option<ColumnType>>, SQLError> {
    SchemaScope::from_context(ctes)?
        .bind_values_types(routines, rows, subqueries, schema, params, schema)
}

pub(super) fn merge_types(
    left: Option<&ColumnType>,
    right: Option<&ColumnType>,
) -> Result<Option<ColumnType>, SQLError> {
    match (left, right) {
        (None, None) => Ok(None),
        (Some(ty), None) | (None, Some(ty)) => Ok(Some(ty.clone())),
        (Some(left), Some(right)) => crate::common_type(left, right).map(Some),
    }
}
