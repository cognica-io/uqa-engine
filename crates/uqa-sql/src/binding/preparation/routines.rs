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
    prepare_routine_body_inputs(
        routines,
        &mut plan.clone(),
        params,
        binding,
        aliases,
        parameters,
    )
}

/// Analyze and retain a SQL body's input constants in a fresh caller-owned plan.
/// This is the same ordered analysis used by creation validation; it does not
/// compile or replace the session's execution-time routine body cache.
pub(crate) fn prepare_routine_body_inputs(
    routines: &dyn RoutineResolution,
    plan: &mut UnifiedPlan,
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
            routines.catalog_input_functions(),
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
    let constants = analysis.parameters.take_input_constants();
    analysis.parameters.finish()?;
    constants.apply(plan)?;
    plan.normalize_window_definitions()?;
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
            routines.catalog_input_functions(),
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

/// Prepare CALL's chosen inputs before publishing a procedural source site.
/// Selection uses the same procedure resolver as ordinary execution.
pub(crate) fn prepare_procedure_call(
    context: &crate::binding::statements::StatementAnalysisContext<'_>,
    overloads: &crate::routines::resolution::RoutineOverloadContext<'_>,
    types: &dyn crate::routines::declaration::RoutineTypeCatalog,
    plan: &mut UnifiedPlan,
    params: &[crate::SQLParam],
    binding: &BindingContext<'_>,
) -> Result<
    (
        crate::binding::statements::AnalyzedResult,
        crate::prepared::dependencies::PreparedAnalysisDependencies,
    ),
    SQLError,
> {
    use crate::{
        ir::{analyze_expression_call_arguments, ScalarExpr},
        plan::CommandPlan,
        routines::{call::ProcedureCallAnalysis, invocation::call_output_schema},
    };
    let UnifiedPlan::Command(command) = &*plan else {
        unreachable!("CALL plan")
    };
    let CommandPlan::Call { name, args } = command.as_ref() else {
        unreachable!("CALL command")
    };
    let declared = super::super::statements::parameter_input_types(params)?;
    let mut scope = SchemaScope::for_analysis(binding)?;
    scope.prepared_dependencies =
        Some(crate::prepared::dependencies::PreparedAnalysisDependencies::default());
    let mut analysis = Preparation {
        routines: context.routines,
        scope,
        parameters: ParameterTypes::with_input_constants(
            &declared,
            Some(context.aliases),
            context.routines.enum_labels(),
            context.routines.catalog_input_functions(),
        ),
        schema_expression: None,
    };
    let resolved = ProcedureCallAnalysis::new(args)?.resolve(name, overloads, &mut |argument| {
        let scalar = crate::ir::scalar_call_argument(&argument.scalar)?.value;
        analysis
            .expression(scalar, &RowSchema::default(), &argument.subqueries)
            .map(|expression| expression.ty)
    })?;
    let (decoded, _) = analyze_expression_call_arguments(args)?;
    for (argument, target) in decoded.iter().zip(&resolved.invocation.argument_targets) {
        if matches!(
            argument.value,
            ScalarExpr::Literal(uqa_core::Value::Str(_) | uqa_core::Value::Null)
        ) {
            let target = types.resolve_catalog_column_type_name(target)?;
            let mut expression = analysis.expression(argument.value, &RowSchema::default(), &[])?;
            analysis
                .parameters
                .coerce_unknown(&mut expression, &target)?;
        }
    }
    let schema = call_output_schema(
        types,
        &resolved.function.def,
        &resolved.invocation.parameter_types,
    )?;
    let constants = analysis.parameters.take_input_constants();
    analysis.parameters.finish()?;
    constants.apply(plan)?;
    let mut dependencies = analysis.scope.prepared_dependencies.unwrap_or_default();
    if let Some(identity) = resolved.function.def.object_id {
        dependencies.routines.insert(identity);
    }
    plan.visit_scalar_expressions(&mut |expression| dependencies.include_expression(expression));
    Ok((
        schema.map_or(
            crate::binding::statements::AnalyzedResult::Command,
            crate::binding::statements::AnalyzedResult::Schema,
        ),
        dependencies,
    ))
}
