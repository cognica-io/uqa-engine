//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Analyze prepared declarations with separate retained inference and descriptor scopes.

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
}

pub fn analyze_definition(
    context: &PreparedDefinitionContext<'_>,
    mut logical_plan: UnifiedPlan,
    declared: &[ColumnType],
) -> Result<PreparedDefinition, SQLError> {
    let parameter_types =
        super::declared_parameter_types(context.types, &mut logical_plan, declared)?;
    let (parameter_types, effective_search_path) = with_scope_result(context.scopes, |scope| {
        let binding = scope.binding_context()?;
        let parameter_types = crate::binding::read_prepared_inputs(
            context.routines,
            &mut logical_plan,
            &parameter_types,
            &binding,
        )?;
        let effective_search_path = binding.catalog.effective_search_path(&binding.resolution)?;
        Ok((parameter_types, effective_search_path))
    })?;
    // Parse analysis reads the names of the statement's `reg*` constants when it is prepared, so a name no object has is reported here.
    crate::schema::dependencies::oid_alias::read_prepared_oid_alias_constants(
        context.aliases,
        &mut logical_plan,
    )?;
    let result_schema = analyze_result_schema(context, &logical_plan, &parameter_types)?;
    Ok(PreparedDefinition {
        logical_plan,
        parameter_types,
        result_schema,
        effective_search_path,
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
