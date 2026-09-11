//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Analyze prepared declarations with separate retained inference and descriptor scopes.

use crate::{
    binding::statements::{StatementAnalysisScopes, StatementBindingScope},
    plan::UnifiedPlan,
    routines::RoutineResolution,
    ColumnType, FunctionTypeResolver, RowSchema, SQLError,
};

#[derive(Clone, Copy)]
pub struct PreparedDefinitionContext<'a> {
    pub types: &'a dyn FunctionTypeResolver,
    pub routines: &'a dyn RoutineResolution,
    pub scopes: &'a dyn StatementAnalysisScopes,
}

pub struct PreparedDefinition {
    pub logical_plan: UnifiedPlan,
    pub parameter_types: Vec<Option<ColumnType>>,
    pub result_schema: Option<RowSchema>,
}

pub fn analyze_definition(
    context: &PreparedDefinitionContext<'_>,
    mut logical_plan: UnifiedPlan,
    declared: &[ColumnType],
) -> Result<PreparedDefinition, SQLError> {
    let parameter_types =
        super::declared_parameter_types(context.types, &mut logical_plan, declared)?;
    let parameter_types = with_scope_result(context.scopes, |scope| {
        crate::binding::infer_prepared_parameter_types(
            context.routines,
            &logical_plan,
            &parameter_types,
            &scope.binding_context()?,
        )
    })?;
    let result_schema = analyze_result_schema(context, &logical_plan, &parameter_types)?;
    Ok(PreparedDefinition {
        logical_plan,
        parameter_types,
        result_schema,
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
