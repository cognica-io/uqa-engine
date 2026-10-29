//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Preparation constructs each executable and cache descriptor in its invoking allowance.

use super::{PreparedDefinition, PreparedDefinitionContext};
use crate::{
    binding::{production::ControlledBindingContext, statements::StatementAnalysisScopes},
    catalog::resolution::EffectiveSearchPath,
    plan::{
        CommandPlan, MutableExecutablePlan, OwnedExecutablePlan, PlanRef, SharedExecutablePlan,
    },
    prepared::{
        dependencies::{PreparedAnalysisDependencies, PreparedDependencySnapshot},
        ownership::SharedPreparedInput,
    },
    ColumnType, RowSchema, SQLError,
};
use uqa_core::memory::{Produced, ProductionControl};

impl PreparedDefinition<OwnedExecutablePlan> {
    pub fn into_shared_with_control(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<PreparedDefinition<SharedExecutablePlan>, SQLError> {
        Ok(PreparedDefinition {
            composite_inputs: self.composite_inputs,
            logical_plan: self.logical_plan.into_shared(control)?,
            parameter_types: self.parameter_types,
            result_schema: self.result_schema,
            effective_search_path: self.effective_search_path,
            dependencies: self.dependencies,
            dependency_snapshot: self.dependency_snapshot,
        })
    }
}

pub fn analyze_definition_owned(
    context: &PreparedDefinitionContext<'_>,
    logical_plan: MutableExecutablePlan,
    declared: &[ColumnType],
    control: &ProductionControl<'_>,
) -> Result<PreparedDefinition<OwnedExecutablePlan>, SQLError> {
    analyze_definition_with_finalizer(context, logical_plan, declared, control, |plan, _| Ok(plan))
}

/// SQL routines validate the result before binding retained type spellings, while the same private destination remains editable.
pub fn analyze_routine_definition_owned(
    context: &PreparedDefinitionContext<'_>,
    types: &dyn crate::routines::declaration::RoutineTypeCatalog,
    logical_plan: MutableExecutablePlan,
    declared: &[ColumnType],
    validate: &dyn Fn(Option<&RowSchema>) -> Result<(), SQLError>,
    control: &ProductionControl<'_>,
) -> Result<PreparedDefinition<OwnedExecutablePlan>, SQLError> {
    analyze_definition_with_finalizer(context, logical_plan, declared, control, |plan, schema| {
        validate(schema)?;
        crate::routines::compilation::bind_analyzed_sql_body_types_owned(types, plan, control)
    })
}

fn analyze_definition_with_finalizer(
    context: &PreparedDefinitionContext<'_>,
    logical_plan: MutableExecutablePlan,
    declared: &[ColumnType],
    control: &ProductionControl<'_>,
    finalize: impl FnOnce(
        MutableExecutablePlan,
        Option<&RowSchema>,
    ) -> Result<MutableExecutablePlan, SQLError>,
) -> Result<PreparedDefinition<OwnedExecutablePlan>, SQLError> {
    let parameter_types = crate::prepared::declared_parameter_types_for_plan_with_control(
        context.types,
        logical_plan.as_plan_ref(),
        declared,
        control,
    )?;
    if !crate::prepared::utility::requires_analysis_ref(logical_plan.as_plan_ref()) {
        let result_schema = crate::prepared::utility::result_schema_with_control(
            context.session,
            logical_plan.as_plan_ref(),
            control,
        )?;
        return Ok(PreparedDefinition {
            composite_inputs: crate::prepared::composites::CompositeInputs::default(),
            logical_plan: finalize(logical_plan, result_schema.as_ref())?.finish(control)?,
            parameter_types: SharedPreparedInput::from_produced(parameter_types, control)?,
            result_schema,
            effective_search_path: shared(None, control)?,
            dependencies: shared(PreparedAnalysisDependencies::default(), control)?,
            dependency_snapshot: shared(None, control)?,
        });
    }
    let mut source = Some(logical_plan);
    let (logical_plan, parameter_types, dependencies, effective_path, snapshot) =
        with_controlled_scope_result(context.scopes, control, |binding| {
            let catalog = binding.catalog.clone();
            let resolution = binding.resolution.copy_with_control(control)?;
            let plan = source.take().ok_or_else(|| {
                SQLError::Internal("prepared analysis scope invoked its operation twice".into())
            })?;
            let input = read_definition_inputs(context, plan, &parameter_types, binding, control)?;
            let plan =
                crate::schema::dependencies::oid_alias::read_prepared_oid_alias_constants_owned(
                    context.aliases,
                    input.plan,
                    control,
                )?;
            let (mut dependencies, memory) = input.dependencies.into_parts();
            let mut result = Ok(());
            let mut include = |expression: &crate::ScalarExpr| {
                if result.is_ok() {
                    result = dependencies.include_expression_with_control(expression, control);
                }
            };
            match plan.as_plan_ref() {
                PlanRef::Query(query) => query.visit_scalar_expressions(&mut include),
                PlanRef::Command(command) => command.visit_scalar_expressions(&mut include),
            }
            result?;
            let path = catalog.effective_search_path_with_control(&resolution, control)?;
            let snapshot =
                catalog.prepared_dependency_snapshot_with_control(&dependencies, control)?;
            Ok((
                plan,
                input.parameter_types,
                control.finish(dependencies, memory)?,
                path,
                snapshot,
            ))
        })?;
    let result_schema = analyze_result_schema_with_control(
        context,
        logical_plan.as_plan_ref(),
        &parameter_types,
        control,
    )?;
    let composite_inputs =
        crate::prepared::composites::CompositeInputs::capture_for_plan_with_control(
            logical_plan.as_plan_ref(),
            context.types,
            control,
        )?;
    let logical_plan = finalize(logical_plan, result_schema.as_ref())?.finish(control)?;
    Ok(PreparedDefinition {
        composite_inputs,
        logical_plan,
        parameter_types: SharedPreparedInput::from_produced(parameter_types, control)?,
        result_schema,
        effective_search_path: SharedPreparedInput::from_produced(effective_path, control)?,
        dependencies: SharedPreparedInput::from_produced(dependencies, control)?,
        dependency_snapshot: shared(snapshot, control)?,
    })
}

fn shared<T>(
    value: T,
    control: &ProductionControl<'_>,
) -> Result<SharedPreparedInput<T>, SQLError> {
    SharedPreparedInput::from_produced(control.finish(value, control.empty_reservation())?, control)
}

pub fn analysis_is_current_with_control(
    context: &PreparedDefinitionContext<'_>,
    retained_path: Option<&EffectiveSearchPath>,
    dependencies: &PreparedAnalysisDependencies,
    retained_snapshot: Option<&PreparedDependencySnapshot>,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    control.check()?;
    if retained_path.is_none() && retained_snapshot.is_none() {
        return Ok(true);
    }
    with_controlled_scope_result(context.scopes, control, |binding| {
        if let Some(path) = retained_path {
            if binding
                .catalog
                .effective_search_path_with_control(&binding.resolution, control)?
                .as_ref()
                != Some(path)
            {
                return Ok(false);
            }
        }
        match retained_snapshot {
            Some(snapshot) => Ok(binding
                .catalog
                .prepared_dependency_snapshot_with_control(dependencies, control)?
                .as_ref()
                == Some(snapshot)),
            None => Ok(true),
        }
    })
}

pub fn effective_search_path_with_control(
    context: &PreparedDefinitionContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Option<EffectiveSearchPath>>, SQLError> {
    with_controlled_scope_result(context.scopes, control, |binding| {
        binding
            .catalog
            .effective_search_path_with_control(&binding.resolution, control)
    })
}

pub fn analyze_result_schema_with_control(
    context: &PreparedDefinitionContext<'_>,
    plan: PlanRef<'_>,
    parameter_types: &[Option<ColumnType>],
    control: &ProductionControl<'_>,
) -> Result<Option<RowSchema>, SQLError> {
    if !crate::prepared::utility::requires_analysis_ref(plan) {
        return crate::prepared::utility::result_schema_with_control(
            context.session,
            plan,
            control,
        );
    }
    with_controlled_scope_result(context.scopes, control, |binding| {
        let params = crate::prepared::analysis_parameters_with_control(parameter_types, control)?;
        if let PlanRef::Command(CommandPlan::Call { name, args }) = plan {
            return crate::routines::call::OwnedProcedureCallAnalysis::new(args, control)?
                .result_schema_with_control(
                    name,
                    &crate::routines::resolution::RoutineOverloadContext {
                        catalog: context.procedures,
                    },
                    context.procedures,
                    &mut |argument| {
                        crate::binding::production::analyze_expression_plan_type_with_control(
                            context.routines,
                            argument,
                            &params,
                            binding.copy_with_control(control)?,
                            control,
                        )
                    },
                    control,
                );
        }
        match plan {
            PlanRef::Query(query) => {
                crate::binding::production::analyze_query_plan_schema_with_control(
                    context.routines,
                    query,
                    &params,
                    binding,
                    None,
                    control,
                )
                .map(Some)
            }
            PlanRef::Command(command) => {
                crate::binding::analyze_prepared_command_schema_with_control(
                    context.routines,
                    command,
                    &params,
                    binding,
                    control,
                )
            }
        }
    })
}

fn with_controlled_scope_result<T>(
    scopes: &dyn StatementAnalysisScopes,
    control: &ProductionControl<'_>,
    mut analyze: impl FnMut(ControlledBindingContext<'_>) -> Result<T, SQLError>,
) -> Result<T, SQLError> {
    let mut result = None;
    scopes.with_controlled_scope(control, &mut |binding| {
        result = Some(analyze(binding)?);
        Ok(())
    })?;
    result.ok_or_else(|| {
        SQLError::Internal("prepared analysis scope did not invoke its operation".into())
    })
}

fn read_definition_inputs(
    context: &PreparedDefinitionContext<'_>,
    plan: MutableExecutablePlan,
    parameter_types: &[Option<ColumnType>],
    binding: ControlledBindingContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<crate::binding::PreparedOwnedInputs, SQLError> {
    Ok(
        if matches!(
            plan.as_plan_ref(),
            PlanRef::Command(CommandPlan::Call { .. })
        ) {
            let metadata_binding = binding.copy_with_control(control)?;
            let (_, mut input) = crate::binding::read_procedure_inputs_owned(
                &crate::binding::statements::StatementAnalysisContext {
                    routines: context.routines,
                    scopes: context.scopes,
                    aliases: context.aliases,
                },
                &crate::routines::resolution::RoutineOverloadContext {
                    catalog: context.procedures,
                },
                context.procedures,
                plan,
                parameter_types,
                binding,
                control,
            )?;
            input.plan = crate::binding::retain_prepared_input_metadata_owned(
                context.routines,
                input.plan,
                &input.parameter_types,
                metadata_binding,
                control,
            )?;
            input
        } else {
            crate::binding::read_prepared_inputs_owned(
                context.routines,
                plan,
                parameter_types,
                binding,
                Some(context.aliases),
                control,
            )?
        },
    )
}
