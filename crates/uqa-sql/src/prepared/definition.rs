//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Analyze prepared declarations with separate retained inference and descriptor scopes.

use super::dependencies::{PreparedAnalysisDependencies, PreparedDependencySnapshot};
use super::ownership::SharedPreparedInput;
use crate::{
    binding::statements::{StatementAnalysisScopes, StatementBindingScope},
    catalog::resolution::EffectiveSearchPath,
    plan::UnifiedPlan,
    routines::RoutineResolution,
    ColumnType, FunctionTypeResolver, RowSchema, SQLError,
};

mod production;
pub use production::{
    analysis_is_current_with_control, analyze_definition_owned, analyze_result_schema_with_control,
    analyze_routine_definition_owned, effective_search_path_with_control,
};

#[derive(Clone, Copy)]
pub struct PreparedDefinitionContext<'a> {
    pub types: &'a dyn FunctionTypeResolver,
    pub routines: &'a dyn RoutineResolution,
    pub procedures: &'a dyn crate::routines::resolution::RoutineOverloadCatalog,
    pub scopes: &'a dyn StatementAnalysisScopes,
    pub session: &'a dyn super::utility::PreparedSessionMetadata,
    /// The catalog the `reg*` input functions read object names from when the statement is analyzed.
    pub aliases: &'a dyn crate::schema::dependencies::oid_alias::OidAliasInput,
}

pub struct PreparedDefinition<P = UnifiedPlan> {
    pub composite_inputs: super::composites::CompositeInputs,
    pub logical_plan: P,
    pub parameter_types: SharedPreparedInput<Vec<Option<ColumnType>>>,
    pub result_schema: Option<RowSchema>,
    pub effective_search_path: SharedPreparedInput<Option<EffectiveSearchPath>>,
    pub dependencies: SharedPreparedInput<PreparedAnalysisDependencies>,
    pub dependency_snapshot: SharedPreparedInput<Option<PreparedDependencySnapshot>>,
}

impl PreparedDefinition {
    /// Publish ordinary analysis storage without copying its tree. Admitted producers publish their existing shared owner directly.
    pub fn into_shared(self) -> PreparedDefinition<crate::plan::SharedExecutablePlan> {
        PreparedDefinition {
            composite_inputs: self.composite_inputs,
            logical_plan: self.logical_plan.into(),
            parameter_types: self.parameter_types,
            result_schema: self.result_schema,
            effective_search_path: self.effective_search_path,
            dependencies: self.dependencies,
            dependency_snapshot: self.dependency_snapshot,
        }
    }
}

pub fn analyze_definition(
    context: &PreparedDefinitionContext<'_>,
    mut logical_plan: UnifiedPlan,
    declared: &[ColumnType],
) -> Result<PreparedDefinition, SQLError> {
    let parameter_types =
        super::declared_parameter_types(context.types, &mut logical_plan, declared)?;
    if !super::utility::requires_analysis(&logical_plan) {
        let result_schema = super::utility::result_schema(context.session, &logical_plan)?;
        return Ok(PreparedDefinition {
            composite_inputs: super::composites::CompositeInputs::default(),
            logical_plan,
            parameter_types: parameter_types.into(),
            result_schema,
            effective_search_path: None.into(),
            dependencies: PreparedAnalysisDependencies::default().into(),
            dependency_snapshot: None.into(),
        });
    }
    let (input, effective_search_path, dependency_snapshot) = with_scope_result(
        context.scopes,
        |scope| {
            let binding = scope.binding_context()?;
            let mut input = if matches!(&logical_plan, UnifiedPlan::Command(command) if matches!(command.as_ref(), crate::plan::CommandPlan::Call { .. }))
            {
                let (_, input) = crate::binding::read_procedure_inputs(
                    &crate::binding::statements::StatementAnalysisContext {
                        routines: context.routines,
                        scopes: context.scopes,
                        aliases: context.aliases,
                    },
                    &crate::routines::resolution::RoutineOverloadContext {
                        catalog: context.procedures,
                    },
                    context.procedures,
                    &mut logical_plan,
                    &parameter_types,
                    &binding,
                )?;
                crate::binding::retain_prepared_input_metadata(
                    context.routines,
                    &mut logical_plan,
                    &input.parameter_types,
                    &binding,
                )?;
                input
            } else {
                crate::binding::read_prepared_inputs(
                    context.routines,
                    &mut logical_plan,
                    &parameter_types,
                    &binding,
                    Some(context.aliases),
                )?
            };
            crate::schema::dependencies::oid_alias::read_prepared_oid_alias_constants(
                context.aliases,
                &mut logical_plan,
            )?;
            logical_plan.visit_scalar_expressions(&mut |expression| {
                input.dependencies.include_expression(expression);
            });
            let effective_search_path =
                binding.catalog.effective_search_path(&binding.resolution)?;
            let dependency_snapshot = binding
                .catalog
                .prepared_dependency_snapshot(&input.dependencies)?;
            Ok((input, effective_search_path, dependency_snapshot))
        },
    )?;
    let result_schema = analyze_result_schema(context, &logical_plan, &input.parameter_types)?;
    Ok(PreparedDefinition {
        composite_inputs: super::composites::CompositeInputs::capture(
            &logical_plan,
            context.types,
        )?,
        logical_plan,
        parameter_types: input.parameter_types.into(),
        result_schema,
        effective_search_path: effective_search_path.into(),
        dependencies: input.dependencies.into(),
        dependency_snapshot: dependency_snapshot.into(),
    })
}

/// Check analysis dependencies in one retained namespace. Catalogs that supplied no path or revision capability at analysis leave that part of freshness to their owner's invalidation events.
pub fn analysis_is_current(
    context: &PreparedDefinitionContext<'_>,
    retained_path: Option<&EffectiveSearchPath>,
    dependencies: &PreparedAnalysisDependencies,
    retained_snapshot: Option<&PreparedDependencySnapshot>,
) -> Result<bool, SQLError> {
    if retained_path.is_none() && retained_snapshot.is_none() {
        return Ok(true);
    }
    with_scope_result(context.scopes, |scope| {
        let binding = scope.binding_context()?;
        if let Some(path) = retained_path {
            if binding
                .catalog
                .effective_search_path(&binding.resolution)?
                .as_ref()
                != Some(path)
            {
                return Ok(false);
            }
        }
        match retained_snapshot {
            Some(snapshot) => Ok(binding
                .catalog
                .prepared_dependency_snapshot(dependencies)?
                .as_ref()
                == Some(snapshot)),
            None => Ok(true),
        }
    })
}

/// Read the live namespace through one retained binding scope before selecting a previously analyzed prepared statement.
pub fn effective_search_path(
    context: &PreparedDefinitionContext<'_>,
) -> Result<Option<EffectiveSearchPath>, SQLError> {
    with_scope_result(context.scopes, |scope| {
        let binding = scope.binding_context()?;
        binding.catalog.effective_search_path(&binding.resolution)
    })
}

pub fn analyze_result_schema(
    context: &PreparedDefinitionContext<'_>,
    logical_plan: &UnifiedPlan,
    parameter_types: &[Option<ColumnType>],
) -> Result<Option<RowSchema>, SQLError> {
    if !super::utility::requires_analysis(logical_plan) {
        return super::utility::result_schema(context.session, logical_plan);
    }
    with_scope_result(context.scopes, |scope| {
        let binding = scope.binding_context()?;
        if let UnifiedPlan::Command(command) = logical_plan {
            if let crate::plan::CommandPlan::Call { name, args } = command.as_ref() {
                let params = super::analysis_parameters(parameter_types);
                return crate::routines::call::ProcedureCallAnalysis::new(args)?.result_schema(
                    name,
                    &crate::routines::resolution::RoutineOverloadContext {
                        catalog: context.procedures,
                    },
                    context.procedures,
                    &mut |argument| {
                        crate::binding::analyze_expression_plan_type(
                            context.routines,
                            argument,
                            &params,
                            &binding,
                        )
                    },
                );
            }
        }
        super::analyze_prepared_plan(context.routines, logical_plan, parameter_types, &binding)
    })
}

fn with_scope_result<T>(
    scopes: &dyn StatementAnalysisScopes,
    mut analyze: impl FnMut(&dyn StatementBindingScope) -> Result<T, SQLError>,
) -> Result<T, SQLError> {
    let mut result = None;
    scopes.with_scope(&mut |scope| {
        result = Some(analyze(scope)?);
        Ok(())
    })?;
    result.ok_or_else(|| {
        SQLError::Internal("prepared analysis scope did not invoke its operation".into())
    })
}

#[cfg(test)]
mod tests;
