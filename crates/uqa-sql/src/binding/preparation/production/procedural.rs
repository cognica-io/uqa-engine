//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reached procedural statements retain converted inputs and selected dependencies in their own executable destination.

use super::{
    include_plan_dependencies, retain_input_metadata_owned, AnalyzedResult, ColumnType,
    ControlledBindingContext, MutableExecutablePlan, OidAliasInput, ParameterTypes, PlanRef,
    Preparation, PreparedAnalysisDependencies, Produced, ProductionControl, ProductionVec,
    RoutineParameterScope, RoutineResolution, SQLError, SQLParam, SchemaScope,
};
use crate::plan::CommandPlan;
use uqa_core::Value;

pub struct ProceduralOwnedInputs {
    pub plan: MutableExecutablePlan,
    pub result: AnalyzedResult,
    pub reusable: bool,
    pub dependencies: Produced<PreparedAnalysisDependencies>,
}

pub fn parameter_input_types_with_control(
    params: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<Option<ColumnType>>>, SQLError> {
    let schema = crate::RowSchema::empty_with_control(control)?;
    let mut types = ProductionVec::new(*control);
    types.reserve(params.len())?;
    for (index, parameter) in params.iter().enumerate() {
        let selected = match parameter {
            SQLParam::Scalar(Value::Str(_) | Value::Null) => None,
            _ => crate::scalar_type_with_control(
                &crate::ScalarExpr::Param(index + 1),
                &schema,
                params,
                control,
            )?,
        };
        let (selected, memory) = match selected {
            Some(ty) => {
                let (ty, memory) = ty.into_parts();
                (Some(ty), memory)
            }
            None => (None, control.empty_reservation()),
        };
        types.push_produced(control.finish(selected, memory)?)?;
    }
    Ok(types.finish()?)
}

pub fn read_procedural_inputs_owned(
    routines: &dyn RoutineResolution,
    plan: MutableExecutablePlan,
    params: &[SQLParam],
    binding: ControlledBindingContext<'_>,
    aliases: &dyn OidAliasInput,
    control: &ProductionControl<'_>,
) -> Result<ProceduralOwnedInputs, SQLError> {
    let declared = parameter_input_types_with_control(params, control)?;
    read_inputs_owned(
        routines,
        plan,
        DeclaredInputs {
            values: params,
            types: &declared,
        },
        binding,
        aliases,
        true,
        control,
    )
}

pub fn read_executable_inputs_owned(
    routines: &dyn RoutineResolution,
    plan: MutableExecutablePlan,
    params: &[SQLParam],
    binding: ControlledBindingContext<'_>,
    aliases: &dyn OidAliasInput,
    control: &ProductionControl<'_>,
) -> Result<ProceduralOwnedInputs, SQLError> {
    let mut declared = ProductionVec::new(*control);
    declared.reserve(params.len())?;
    for parameter in params {
        let selected =
            crate::params::bind_parameter_type_with_control(parameter, routines, control)?;
        let (selected, memory) = match selected {
            Some(ty) => {
                let (ty, memory) = ty.into_parts();
                (Some(ty), memory)
            }
            None => (None, control.empty_reservation()),
        };
        declared.push_produced(control.finish(selected, memory)?)?;
    }
    read_inputs_owned(
        routines,
        plan,
        DeclaredInputs {
            values: params,
            types: &declared.finish()?,
        },
        binding,
        aliases,
        false,
        control,
    )
}

struct DeclaredInputs<'a> {
    values: &'a [SQLParam],
    types: &'a [Option<ColumnType>],
}

fn read_inputs_owned(
    routines: &dyn RoutineResolution,
    mut plan: MutableExecutablePlan,
    inputs: DeclaredInputs<'_>,
    binding: ControlledBindingContext<'_>,
    aliases: &dyn OidAliasInput,
    track_dependencies: bool,
    control: &ProductionControl<'_>,
) -> Result<ProceduralOwnedInputs, SQLError> {
    let DeclaredInputs {
        values: params,
        types: declared,
    } = inputs;
    let metadata = binding.copy_with_control(control)?;
    let mut scope = SchemaScope::for_analysis_with_control(binding, control)?;
    scope.prepared_dependencies = track_dependencies.then(PreparedAnalysisDependencies::default);
    let mut analysis = Preparation {
        bound_condition_queries: None,
        routines,
        scope: &mut scope,
        parameters: ParameterTypes::with_input_constants_with_control(
            declared,
            Some(aliases),
            routines.enum_labels(),
            routines.catalog_input_functions(),
            control,
        )?
        .retain_result_metadata(),
        schema_expression: None,
    };
    let mut finish_parameters = false;
    let result = match plan.as_plan_ref() {
        PlanRef::Query(query) => AnalyzedResult::Schema(analysis.query(query, None)?),
        PlanRef::Command(command) => match command {
            CommandPlan::CreateView { .. }
            | CommandPlan::CreateTableAs { .. }
            | CommandPlan::CreateMaterializedView { .. }
            | CommandPlan::DeclareCursor { .. } => {
                analysis.command(command)?;
                AnalyzedResult::Command
            }
            _ if command.mutation_target().is_some() => {
                finish_parameters = true;
                analysis
                    .command(command)?
                    .map_or(AnalyzedResult::Command, AnalyzedResult::Schema)
            }
            _ => AnalyzedResult::Command,
        },
    };
    let constants = analysis.parameters.take_input_constants();
    if finish_parameters {
        analysis.parameters.finish_produced()?;
    }
    let reusable = constants.reusable_across_messages();
    constants.apply_owned(&mut plan, routines, params, control)?;
    let plan = retain_input_metadata_owned(routines, plan, params, metadata, control)?;
    let mut dependencies = scope.prepared_dependencies.take().unwrap_or_default();
    if track_dependencies {
        include_plan_dependencies(&mut dependencies, &plan, control)?;
    }
    Ok(ProceduralOwnedInputs {
        plan,
        result,
        reusable,
        dependencies: control.finish(dependencies, control.empty_reservation())?,
    })
}

pub fn bind_routine_parameter_references_owned(
    routines: &dyn RoutineResolution,
    mut plan: MutableExecutablePlan,
    params: &[SQLParam],
    binding: ControlledBindingContext<'_>,
    parameters: &RoutineParameterScope,
    control: &ProductionControl<'_>,
) -> Result<MutableExecutablePlan, SQLError> {
    let mut scope = SchemaScope::for_analysis_with_control(binding, control)?;
    scope.routine_parameters = Some(parameters.clone());
    scope.scalar_binding = crate::binding::ScalarBindingMode::References;
    scope.preserve_syntax_shape = true;
    let mut analysis = Preparation {
        bound_condition_queries: None,
        routines,
        scope: &mut scope,
        parameters: ParameterTypes::fixed_with_control(params, control)?.with_reference_retention(),
        schema_expression: None,
    };
    analysis.observe_owned_plan(&plan, Some(parameters.schema()))?;
    let references = analysis.parameters.take_reference_constants();
    references.apply_owned(&mut plan, routines, params, control)?;
    Ok(plan)
}
