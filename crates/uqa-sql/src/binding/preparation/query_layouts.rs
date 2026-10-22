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
    analysis.parameters.apply_query_schemas(query);
    Ok(())
}
