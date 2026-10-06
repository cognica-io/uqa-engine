//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered routine-body input analysis with the routine parameter namespace.

use super::{
    BindingContext, ColumnType, OidAliasInput, ParameterTypes, Preparation, RoutineResolution,
    RowSchema, SQLError, SchemaScope, UnifiedPlan,
};

/// Analyze a routine statement's inputs without executing it or retaining the
/// constants in its source. Its target list resolves unknown literals to text,
/// just as an ordinary analyzed statement does before SQL return validation.
pub(crate) fn analyze_routine_body_inputs(
    routines: &dyn RoutineResolution,
    plan: &UnifiedPlan,
    params: &[crate::SQLParam],
    binding: &BindingContext<'_>,
    aliases: &dyn OidAliasInput,
    parameters: Option<&crate::binding::RoutineParameterScope>,
) -> Result<crate::binding::statements::AnalyzedResult, SQLError> {
    let declared = params
        .iter()
        .map(|parameter| parameter.declared_scalar_type().cloned())
        .collect::<Vec<_>>();
    let mut analysis = Preparation {
        routines,
        scope: SchemaScope::for_analysis(binding)?,
        parameters: ParameterTypes::with_input_constants(
            &declared,
            Some(aliases),
            routines.enum_labels(),
        ),
        schema_expression: None,
    };
    analysis.scope.routine_parameters = parameters.cloned();
    let result = match plan {
        UnifiedPlan::Query(query) => Some(analysis.query(
            query,
            parameters.map(crate::binding::RoutineParameterScope::schema),
        )?),
        UnifiedPlan::Command(command) => analysis.command(command)?,
    };
    analysis.parameters.finish()?;
    Ok(result.map_or(
        crate::binding::statements::AnalyzedResult::Command,
        |schema| crate::binding::statements::AnalyzedResult::Schema(schema),
    ))
}

/// Analyze an argument at its written position, optionally applying its selected
/// parameter type to an unknown literal. This never evaluates an ordinary cast.
pub(crate) fn analyze_routine_body_argument(
    routines: &dyn RoutineResolution,
    expression: &crate::plan::ExpressionPlan,
    params: &[crate::SQLParam],
    binding: &BindingContext<'_>,
    aliases: &dyn OidAliasInput,
    target: Option<&ColumnType>,
) -> Result<Option<ColumnType>, SQLError> {
    let declared = params
        .iter()
        .map(|parameter| parameter.declared_scalar_type().cloned())
        .collect::<Vec<_>>();
    let mut analysis = Preparation {
        routines,
        scope: SchemaScope::for_analysis(binding)?,
        parameters: ParameterTypes::with_input_constants(
            &declared,
            Some(aliases),
            routines.enum_labels(),
        ),
        schema_expression: None,
    };
    let mut observed = analysis.expression(
        &expression.scalar,
        &RowSchema::default(),
        &expression.subqueries,
    )?;
    if let Some(target) = target {
        analysis.parameters.coerce_unknown(&mut observed, target)?;
    }
    analysis.parameters.finish()?;
    Ok(observed.ty)
}
