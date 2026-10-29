//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered analysis owns its executable destination, parameter observations and published dependency nodes independently.

use super::{ParameterTypes, Preparation, RoutineResolution, SQLError, SchemaScope};
use crate::{
    binding::{
        production::ControlledBindingContext, statements::AnalyzedResult, RoutineParameterScope,
    },
    plan::{MutableExecutablePlan, PlanRef, RelationalPlan},
    prepared::dependencies::PreparedAnalysisDependencies,
    schema::dependencies::oid_alias::OidAliasInput,
    ColumnType, SQLParam,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

mod procedural;
pub use procedural::{
    bind_routine_parameter_references_owned, parameter_input_types_with_control,
    read_executable_inputs_owned, read_procedural_inputs_owned, ProceduralOwnedInputs,
};
mod explain;
pub use explain::read_generic_explain_inputs_owned;
mod stored;
pub use stored::bind_expression_plan_routines_for_storage_owned;

pub struct PreparedOwnedInputs {
    pub plan: MutableExecutablePlan,
    pub parameter_types: Produced<Vec<Option<ColumnType>>>,
    pub dependencies: Produced<PreparedAnalysisDependencies>,
}

pub struct PreparedOwnedRoutineBody {
    pub plan: MutableExecutablePlan,
    pub result: AnalyzedResult,
}

pub fn read_prepared_inputs_owned(
    routines: &dyn RoutineResolution,
    mut plan: MutableExecutablePlan,
    declared: &[Option<ColumnType>],
    binding: ControlledBindingContext<'_>,
    aliases: Option<&dyn OidAliasInput>,
    control: &ProductionControl<'_>,
) -> Result<PreparedOwnedInputs, SQLError> {
    let metadata_binding = binding.copy_with_control(control)?;
    let mut scope = SchemaScope::for_analysis_with_control(binding, control)?;
    scope.prepared_dependencies = Some(PreparedAnalysisDependencies::default());
    let mut analysis = Preparation {
        bound_condition_queries: None,
        routines,
        scope: &mut scope,
        parameters: ParameterTypes::with_input_constants_with_control(
            declared,
            aliases,
            routines.enum_labels(),
            routines.catalog_input_functions(),
            control,
        )?
        .retain_result_metadata(),
        schema_expression: None,
    };
    analysis.observe_owned_plan(&plan, None)?;
    let constants = analysis.parameters.take_input_constants();
    let parameters = analysis.parameters.finish_produced()?;
    let params = crate::prepared::analysis_parameters_with_control(&parameters, control)?;
    constants.apply_owned(&mut plan, routines, &params, control)?;
    let plan = retain_prepared_input_metadata_owned(
        routines,
        plan,
        &parameters,
        metadata_binding,
        control,
    )?;
    let mut dependencies = scope.prepared_dependencies.take().unwrap_or_default();
    include_plan_dependencies(&mut dependencies, &plan, control)?;
    Ok(PreparedOwnedInputs {
        plan,
        parameter_types: parameters,
        dependencies: control.finish(dependencies, control.empty_reservation())?,
    })
}

pub fn prepare_routine_body_inputs_owned(
    routines: &dyn RoutineResolution,
    mut plan: MutableExecutablePlan,
    params: &[SQLParam],
    binding: ControlledBindingContext<'_>,
    aliases: &dyn OidAliasInput,
    parameters: Option<&RoutineParameterScope>,
    control: &ProductionControl<'_>,
) -> Result<PreparedOwnedRoutineBody, SQLError> {
    let declared = declared_inputs(params, control)?;
    let mut scope = SchemaScope::for_analysis_with_control(binding, control)?;
    scope.routine_parameters = parameters.cloned();
    let mut analysis = Preparation {
        bound_condition_queries: None,
        routines,
        scope: &mut scope,
        parameters: ParameterTypes::with_input_constants_with_control(
            &declared,
            Some(aliases),
            routines.enum_labels(),
            routines.catalog_input_functions(),
            control,
        )?
        .with_routine_parameter_binding(),
        schema_expression: None,
    };
    let result =
        analysis.observe_owned_plan(&plan, parameters.map(RoutineParameterScope::schema))?;
    let constants = analysis.parameters.take_input_constants();
    analysis.parameters.finish_produced()?;
    constants.apply_owned(&mut plan, routines, params, control)?;
    let plan = normalize_windows(plan, control)?;
    Ok(PreparedOwnedRoutineBody { plan, result })
}

pub(super) fn normalize_windows(
    plan: MutableExecutablePlan,
    control: &ProductionControl<'_>,
) -> Result<MutableExecutablePlan, SQLError> {
    plan.normalize_window_definitions(control)
}

pub(super) fn retain_compute_kinds_owned(
    plan: MutableExecutablePlan,
    routines: &dyn RoutineResolution,
    control: &ProductionControl<'_>,
) -> Result<MutableExecutablePlan, SQLError> {
    struct Aggregates<'a>(&'a dyn RoutineResolution);
    impl crate::plan::AggregateClassifier for Aggregates<'_> {
        fn is_registered_aggregate(&self, name: &str) -> bool {
            self.0.has_registered_aggregate_function(name)
        }
        fn is_registered_aggregate_with_control(
            &self,
            name: &str,
            control: &ProductionControl<'_>,
        ) -> Result<bool, SQLError> {
            self.0
                .has_registered_aggregate_function_with_control(name, control)
        }
    }
    plan.map_queries_while(control, &mut |query| {
        let compute = match &query.root {
            RelationalPlan::QueryBlock(block) => {
                Some(block.compute_kind_with_control(&Aggregates(routines), control)?)
            }
            _ => None,
        };
        Ok((
            match compute {
                Some(compute) => query.set_compute(compute, control)?,
                None => query,
            },
            true,
        ))
    })
}

fn declared_inputs(
    params: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<Option<ColumnType>>>, SQLError> {
    let mut declared = ProductionVec::new(*control);
    declared.reserve(params.len())?;
    for parameter in params {
        let (ty, memory) = parameter.declared_scalar_type().map_or_else(
            || Ok::<_, SQLError>((None, control.empty_reservation())),
            |ty| {
                let (ty, memory) = ty.clone_with_control(control)?.into_parts();
                Ok((Some(ty), memory))
            },
        )?;
        declared.push_produced(control.finish(ty, memory)?)?;
    }
    Ok(declared.finish()?)
}

fn include_plan_dependencies(
    dependencies: &mut PreparedAnalysisDependencies,
    plan: &MutableExecutablePlan,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let mut status = Ok(());
    let mut include = |expression: &crate::ScalarExpr| {
        if status.is_ok() {
            status = dependencies.include_expression_with_control(expression, control);
        }
    };
    match plan.as_plan_ref() {
        PlanRef::Query(query) => query.visit_scalar_expressions(&mut include),
        PlanRef::Command(command) => command.visit_scalar_expressions(&mut include),
    }
    status
}

impl Preparation<'_> {
    pub(super) fn observe_owned_plan(
        &mut self,
        plan: &MutableExecutablePlan,
        outer: Option<&crate::RowSchema>,
    ) -> Result<AnalyzedResult, SQLError> {
        let result = match plan.as_plan_ref() {
            PlanRef::Query(query) => Some(self.query(query, outer)?),
            PlanRef::Command(command) => self.command(command)?,
        };
        Ok(result.map_or(AnalyzedResult::Command, AnalyzedResult::Schema))
    }
}

/// Infer and retain CALL inputs in written order, keeping decoded argument sites borrowed until every selected constant has been recorded.
pub fn read_procedure_inputs_owned(
    context: &crate::binding::statements::StatementAnalysisContext<'_>,
    overloads: &crate::routines::resolution::RoutineOverloadContext<'_>,
    types: &dyn crate::routines::declaration::RoutineTypeCatalog,
    mut plan: MutableExecutablePlan,
    declared: &[Option<ColumnType>],
    binding: ControlledBindingContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<(Option<crate::RowSchema>, PreparedOwnedInputs), SQLError> {
    let PlanRef::Command(crate::plan::CommandPlan::Call { name, args }) = plan.as_plan_ref() else {
        return Err(SQLError::Internal(
            "procedure input analysis requires CALL".into(),
        ));
    };
    let mut scope = SchemaScope::for_analysis_with_control(binding, control)?;
    scope.prepared_dependencies = Some(PreparedAnalysisDependencies::default());
    let mut analysis = Preparation {
        bound_condition_queries: None,
        routines: context.routines,
        scope: &mut scope,
        parameters: ParameterTypes::with_input_constants_with_control(
            declared,
            Some(context.aliases),
            context.routines.enum_labels(),
            context.routines.catalog_input_functions(),
            control,
        )?,
        schema_expression: None,
    };
    analysis.scope.set_placement = crate::semantics::sets::placement::SetPlacement::CallArgument;
    let call = crate::routines::call::OwnedProcedureCallAnalysis::new(args, control)?;
    let mut expressions = super::ownership::AnalysisVec::new(control);
    expressions.reserve(call.decoded().len(), control)?;
    let empty = crate::RowSchema::empty_with_control(control)?;
    for argument in call.decoded() {
        expressions.push(analysis.expression(argument.value, &empty, &[])?, control)?;
    }
    let mut argument_types = ProductionVec::new(*control);
    argument_types.reserve(expressions.len())?;
    for expression in &expressions {
        let (ty, memory) = match &expression.ty {
            Some(ty) => {
                let (ty, memory) = ty.clone_with_control(control)?.into_parts();
                (Some(ty), memory)
            }
            None => (None, control.empty_reservation()),
        };
        argument_types.push_produced(control.finish(ty, memory)?)?;
    }
    let argument_types = argument_types.finish()?;
    let resolved = call.resolve_types(name, overloads, &argument_types, control)?;
    drop(argument_types);
    drop(call);
    for (expression, target) in expressions
        .iter_mut()
        .zip(&resolved.invocation.argument_targets)
    {
        let target = types.resolve_catalog_column_type_name_with_control(target, control)?;
        analysis.parameters.coerce_unknown(expression, &target)?;
    }
    let schema = crate::routines::invocation::call_output_schema_with_control(
        types,
        &resolved.function.def,
        &resolved.invocation.parameter_types,
        control,
    )?;
    let constants = analysis.parameters.take_input_constants();
    let parameter_types = analysis.parameters.finish_produced()?;
    let params = crate::prepared::analysis_parameters_with_control(&parameter_types, control)?;
    constants.apply_owned(&mut plan, context.routines, &params, control)?;
    let mut dependencies = scope.prepared_dependencies.take().unwrap_or_default();
    if let Some(identity) = resolved.function.def.object_id {
        dependencies.include_routine_identity_with_control(identity, control)?;
    }
    include_plan_dependencies(&mut dependencies, &plan, control)?;
    Ok((
        schema,
        PreparedOwnedInputs {
            plan,
            parameter_types,
            dependencies: control.finish(dependencies, control.empty_reservation())?,
        },
    ))
}

/// Retain selected composite identities in their analyzed lexical scopes, then normalize window syntax under the same executable owner.
pub fn retain_prepared_input_metadata_owned(
    routines: &dyn RoutineResolution,
    plan: MutableExecutablePlan,
    parameters: &[Option<ColumnType>],
    binding: ControlledBindingContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<MutableExecutablePlan, SQLError> {
    let params = crate::prepared::analysis_parameters_with_control(parameters, control)?;
    retain_input_metadata_owned(routines, plan, &params, binding, control)
}

pub(in crate::binding) fn retain_input_metadata_owned(
    routines: &dyn RoutineResolution,
    plan: MutableExecutablePlan,
    params: &[SQLParam],
    binding: ControlledBindingContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<MutableExecutablePlan, SQLError> {
    let mut plan = retain_compute_kinds_owned(plan, routines, control)?;
    let mut needed = false;
    let mut inspect = |expression: &crate::ScalarExpr| {
        expression.visit(&mut |node| {
        use crate::{ScalarExpr, ast::FunctionDispatch};
        needed |= matches!(node, ScalarExpr::CompositeRow { .. });
        needed |= matches!(node, ScalarExpr::Cast { expr, .. } if matches!(expr.as_ref(), ScalarExpr::Row(_)));
        needed |= matches!(node, ScalarExpr::Func { binding:Some(binding), .. } if matches!(binding.dispatch, Some(FunctionDispatch::FieldSelect | FunctionDispatch::IsDistinct | FunctionDispatch::ValueNullTest { .. })));
    });
    };
    match plan.as_plan_ref() {
        PlanRef::Query(query) => query.visit_scalar_expressions(&mut inspect),
        PlanRef::Command(command) => command.visit_scalar_expressions(&mut inspect),
    }
    if needed {
        let mut scope = SchemaScope::for_analysis_with_control(binding, control)?;
        scope.scalar_binding = crate::binding::ScalarBindingMode::CompositeInputs;
        scope.preserve_syntax_shape = true;
        let mut analysis = Preparation {
            bound_condition_queries: None,
            routines,
            scope: &mut scope,
            parameters: ParameterTypes::fixed_with_control(params, control)?
                .with_composite_retention(),
            schema_expression: None,
        };
        analysis.observe_owned_plan(&plan, None)?;
        let mut replacements = analysis.parameters.take_composite_inputs();
        plan = plan.transform_scalar_nodes(control, &mut |identity, expression| {
            Ok(replacements.remove(&identity).unwrap_or(expression))
        })?;
        if !replacements.is_empty() {
            return Err(SQLError::Internal(
                "composite observation did not belong to its analyzed plan".into(),
            ));
        }
    }
    normalize_windows(plan, control)
}

/// Inspect procedural name/column conflicts in the owned statement's original scopes, preserving the first conflicting site over later semantic diagnostics.
pub fn resolve_variable_sites_owned(
    routines: &dyn RoutineResolution,
    plan: &MutableExecutablePlan,
    params: &[SQLParam],
    binding: ControlledBindingContext<'_>,
    names: Produced<Vec<crate::ScalarExpr>>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<crate::binding::VariableSiteResolution>>, SQLError> {
    use crate::binding::{variable_sites::VariableSites, VariableSiteResolution};
    let mut resolutions = ProductionVec::new(*control);
    resolutions.reserve(names.len())?;
    for _ in 0..names.len() {
        resolutions.extend_from_slice(&[VariableSiteResolution::Variable])?;
    }
    let mut scope = SchemaScope::for_analysis_with_control(binding, control)?;
    scope.scalar_binding = crate::binding::ScalarBindingMode::References;
    scope.variable_sites = Some(VariableSites {
        names,
        resolutions: resolutions.finish()?,
    });
    let mut analysis = Preparation {
        bound_condition_queries: None,
        routines,
        scope: &mut scope,
        parameters: ParameterTypes::fixed_with_control(params, control)?,
        schema_expression: None,
    };
    let walked = analysis.observe_owned_plan(plan, None);
    let resolutions = scope
        .variable_sites
        .take()
        .expect("procedural name scope")
        .resolutions;
    match walked {
        Ok(_) => Ok(resolutions),
        Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => Err(error),
        Err(_) if resolutions.contains(&VariableSiteResolution::Column) => Ok(resolutions),
        Err(error) => Err(error),
    }
}
