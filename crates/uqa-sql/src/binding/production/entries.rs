//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Controlled schema entries share the same analysis scope and carry its invoking allowance through every nested bind.

use super::ControlledBindingContext;
use crate::{
    plan::{QueryPlan, SourcePlan},
    routines::RoutineResolution,
    RowSchema, SQLError, SQLParam,
};
use uqa_core::memory::ProductionControl;

pub fn analyze_query_plan_schema_with_control(
    routines: &dyn RoutineResolution,
    query: &QueryPlan,
    params: &[SQLParam],
    scope: ControlledBindingContext<'_>,
    outer: Option<&RowSchema>,
    control: &ProductionControl<'_>,
) -> Result<RowSchema, SQLError> {
    super::super::SchemaScope::for_analysis_with_control(scope, control)?
        .bind_query(routines, query, params, outer)
}

pub fn bind_query_plan_schema_with_control(
    routines: &dyn RoutineResolution,
    query: &QueryPlan,
    params: &[SQLParam],
    scope: ControlledBindingContext<'_>,
    outer: Option<&RowSchema>,
    control: &ProductionControl<'_>,
) -> Result<RowSchema, SQLError> {
    super::super::SchemaScope::from_context_with_control(scope, control)?
        .bind_query(routines, query, params, outer)
}

pub fn analyze_source_plan_schema_with_control(
    routines: &dyn RoutineResolution,
    source: &SourcePlan,
    params: &[SQLParam],
    scope: ControlledBindingContext<'_>,
    outer: Option<&RowSchema>,
    control: &ProductionControl<'_>,
) -> Result<RowSchema, SQLError> {
    let subqueries = scope.scalar_subqueries;
    super::super::SchemaScope::for_analysis_with_control(scope, control)?
        .bind_source(routines, source, subqueries, params, outer)
}

pub fn analyze_expression_plan_type_with_control(
    routines: &dyn RoutineResolution,
    plan: &crate::plan::ExpressionPlan,
    params: &[SQLParam],
    scope: ControlledBindingContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<Option<uqa_core::memory::Produced<crate::ColumnType>>, SQLError> {
    let empty = RowSchema::new_with_control(
        control.finish(Vec::new(), control.empty_reservation())?,
        control,
    )?;
    super::super::SchemaScope::for_analysis_with_control(scope, control)?
        .bind_expression_type_with_control(routines, &plan.scalar, &empty, &plan.subqueries, params)
}

/// Bind preceding peers and lexical ancestors with the original schema-analysis mode and allowance.
pub fn bind_source_plan_schema_in_scope_with_control(
    routines: &dyn RoutineResolution,
    source: &SourcePlan,
    params: &[SQLParam],
    scope: ControlledBindingContext<'_>,
    names: crate::binding::SourceScope<'_>,
    control: &ProductionControl<'_>,
) -> Result<RowSchema, SQLError> {
    let subqueries = scope.scalar_subqueries;
    super::super::SchemaScope::from_context_with_control(scope, control)?
        .bind_source_in_scope(routines, source, subqueries, params, names)
}
