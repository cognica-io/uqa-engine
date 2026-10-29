//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored expressions keep their mutable scalar and nested query owners through scoped routine selection.

use super::{
    ControlledBindingContext, MutableExecutablePlan, ParameterTypes, PlanRef, Preparation,
    ProductionControl, RelationalPlan, RoutineResolution, SQLError, SQLParam, SchemaScope,
};
use crate::{plan::MutableExpressionPlan, RowSchema};

pub fn bind_expression_plan_routines_for_storage_owned(
    routines: &dyn RoutineResolution,
    expression: MutableExpressionPlan,
    params: &[SQLParam],
    binding: ControlledBindingContext<'_>,
    input: &RowSchema,
    control: &ProductionControl<'_>,
) -> Result<MutableExpressionPlan, SQLError> {
    let mut plan = MutableExecutablePlan::Query(expression.into_projection_query(control)?);
    let mut scope = SchemaScope::for_analysis_with_control(binding, control)?;
    scope.preserve_syntax_shape = true;
    scope.stored_expression_outer = Some(input.clone());
    let mut analysis = Preparation {
        bound_condition_queries: None,
        routines,
        scope: &mut scope,
        parameters: ParameterTypes::fixed_with_control(params, control)?.with_stored_inputs(),
        schema_expression: None,
    };
    let PlanRef::Query(query) = plan.as_plan_ref() else {
        unreachable!()
    };
    let RelationalPlan::QueryBlock(block) = &query.root else {
        unreachable!()
    };
    for query in &block.subqueries {
        analysis.query(query, Some(input))?;
    }
    analysis.expression(&block.projections[0].expr, input, &block.subqueries)?;
    let constants = analysis.parameters.take_input_constants();
    constants.apply_owned(&mut plan, routines, params, control)?;
    let MutableExecutablePlan::Query(query) = plan else {
        unreachable!()
    };
    let expression = MutableExpressionPlan::from_projection_query(query, control)?;
    expression.map_scalar(control, |expression, arena| {
        let resolver = scope.query_function_type_resolver_for_subqueries(
            routines,
            std::slice::from_ref(&*expression),
            input,
            arena.queries(),
            params,
        )?;
        crate::type_resolution::bind_stored_inputs_owned(
            expression, input, params, &resolver, control,
        )
    })
}
