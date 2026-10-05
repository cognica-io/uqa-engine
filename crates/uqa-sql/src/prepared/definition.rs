//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Analyze prepared declarations with separate retained inference and descriptor scopes.

use super::dependencies::{PreparedAnalysisDependencies, PreparedDependencySnapshot};
use crate::{
    binding::statements::{StatementAnalysisScopes, StatementBindingScope},
    catalog::resolution::EffectiveSearchPath,
    plan::UnifiedPlan,
    routines::RoutineResolution,
    ColumnType, FunctionTypeResolver, RowSchema, SQLError,
};

#[derive(Clone, Copy)]
pub struct PreparedDefinitionContext<'a> {
    pub types: &'a dyn FunctionTypeResolver,
    pub routines: &'a dyn RoutineResolution,
    pub scopes: &'a dyn StatementAnalysisScopes,
    /// The catalog the `reg*` input functions read object names from when the statement is analyzed.
    pub aliases: &'a dyn crate::schema::dependencies::oid_alias::OidAliasInput,
}

pub struct PreparedDefinition {
    pub logical_plan: UnifiedPlan,
    pub parameter_types: Vec<Option<ColumnType>>,
    pub result_schema: Option<RowSchema>,
    pub effective_search_path: Option<EffectiveSearchPath>,
    pub dependencies: PreparedAnalysisDependencies,
    pub dependency_snapshot: Option<PreparedDependencySnapshot>,
}

pub fn analyze_definition(
    context: &PreparedDefinitionContext<'_>,
    mut logical_plan: UnifiedPlan,
    declared: &[ColumnType],
) -> Result<PreparedDefinition, SQLError> {
    let parameter_types =
        super::declared_parameter_types(context.types, &mut logical_plan, declared)?;
    let (input, effective_search_path, dependency_snapshot) =
        with_scope_result(context.scopes, |scope| {
            let binding = scope.binding_context()?;
            let mut input = crate::binding::read_prepared_inputs(
                context.routines,
                &mut logical_plan,
                &parameter_types,
                &binding,
                Some(context.aliases),
            )?;
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
        })?;
    let result_schema = analyze_result_schema(context, &logical_plan, &input.parameter_types)?;
    Ok(PreparedDefinition {
        logical_plan,
        parameter_types: input.parameter_types,
        result_schema,
        effective_search_path,
        dependencies: input.dependencies,
        dependency_snapshot,
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
    with_scope_result(context.scopes, |scope| {
        super::analyze_prepared_plan(
            context.routines,
            logical_plan,
            parameter_types,
            &scope.binding_context()?,
        )
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
