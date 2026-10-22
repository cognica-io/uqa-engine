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

pub(super) fn lateral_join_scope(
    source: &crate::plan::SourcePlan,
    left: &RowSchema,
    outer: Option<&RowSchema>,
    kind: crate::ast::JoinKind,
) -> RowSchema {
    if matches!(
        kind,
        crate::ast::JoinKind::Right | crate::ast::JoinKind::Full
    ) {
        use crate::plan::SourcePlan;
        // PostgreSQL names unaliased range entries for diagnostics without making that name available for qualified lookup.
        let namespace = match source {
            SourcePlan::Table {
                qualifier, alias, ..
            } => alias.as_deref().unwrap_or(qualifier),
            SourcePlan::Function {
                output_name, alias, ..
            } => alias.as_deref().unwrap_or(output_name),
            SourcePlan::FunctionGroup {
                functions, alias, ..
            } => alias.as_deref().unwrap_or_else(|| {
                &functions
                    .first()
                    .expect("bound ROWS FROM source has a member")
                    .output_name
            }),
            SourcePlan::Join { alias, .. } => alias.as_deref().unwrap_or("unnamed_join"),
            SourcePlan::Values { alias, .. } | SourcePlan::Subquery { alias, .. } => {
                alias.as_deref().unwrap_or("unnamed_subquery")
            }
            SourcePlan::RetainedRows(_) => "unnamed_subquery",
        };
        overlay_outer_schema(&RowSchema::without_lateral_access(left, namespace), outer)
    } else {
        overlay_outer_schema(left, outer)
    }
}

pub fn values_types_in_scope(
    routines: &dyn RoutineResolution,
    rows: &[Vec<ScalarExpr>],
    subqueries: &[QueryPlan],
    schema: Option<&RowSchema>,
    params: &[SQLParam],
    ctes: &BindingContext,
) -> Result<Vec<Option<ColumnType>>, SQLError> {
    SchemaScope::from_context(ctes)?.bind_values_types(routines, rows, subqueries, schema, params)
}

/// Preserve the complete analyzed VALUES output metadata for physical execution.
pub fn values_schema_in_scope(
    routines: &dyn RoutineResolution,
    rows: &[Vec<ScalarExpr>],
    subqueries: &[QueryPlan],
    schema: Option<&RowSchema>,
    params: &[SQLParam],
    ctes: &BindingContext,
) -> Result<RowSchema, SQLError> {
    SchemaScope::from_context(ctes)?.bind_values_schema(routines, rows, subqueries, schema, params)
}
