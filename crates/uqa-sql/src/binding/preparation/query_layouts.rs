//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Restore transient layouts without changing stored expression selections.

use super::{
    BindingContext, ParameterTypes, Preparation, QueryPlan, RoutineResolution, RowSchema, SQLError,
    SchemaScope,
};

/// Describe a catalog query in its execution scope before optimizer rewrites. Bound constants and calls stay unchanged; only transient source and result layouts are attached.
pub fn restore_query_layouts(
    routines: &dyn RoutineResolution,
    query: &mut QueryPlan,
    params: &[crate::SQLParam],
    binding: &BindingContext<'_>,
    outer: Option<&RowSchema>,
) -> Result<(), SQLError> {
    let mut scope = SchemaScope::for_analysis(binding)?;
    let mut analysis = Preparation {
        bound_condition_queries: None,
        routines,
        scope: &mut scope,
        parameters: ParameterTypes::fixed(params).with_query_schemas(),
        schema_expression: None,
    };
    analysis.query(query, outer)?;
    analysis
        .parameters
        .apply_query_schemas(query, routines, params)
}

/// Attach transient layouts to a private admitted query without copying its tree.
pub fn restore_query_layouts_owned(
    routines: &dyn RoutineResolution,
    query: crate::plan::MutableQueryPlan,
    params: &[crate::SQLParam],
    binding: crate::binding::production::ControlledBindingContext<'_>,
    outer: Option<&RowSchema>,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<crate::plan::MutableQueryPlan, SQLError> {
    use crate::plan::MutableExecutablePlan;
    let mut scope = SchemaScope::for_analysis_with_control(binding, control)?;
    let mut analysis = Preparation {
        bound_condition_queries: None,
        routines,
        scope: &mut scope,
        parameters: ParameterTypes::fixed_with_control(params, control)?.with_query_schemas(),
        schema_expression: None,
    };
    let mut plan = MutableExecutablePlan::Query(query);
    analysis.observe_owned_plan(&plan, outer)?;
    analysis
        .parameters
        .apply_owned_query_schemas(&mut plan, routines, params, control)?;
    let MutableExecutablePlan::Query(query) = plan else {
        unreachable!("layout observation preserves query kind")
    };
    Ok(query)
}
