//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Executable statement schemas and mutation parameter analysis.

use super::BindingContext;
use crate::{
    plan::{CommandPlan, UnifiedPlan},
    routines::RoutineResolution,
    RowSchema, SQLError, SQLParam, ScalarExpr,
};
use uqa_core::Value;

/// Borrow binding inputs only when the statement's semantic branch requires them.
pub trait StatementBindingScope {
    fn binding_context(&self) -> Result<BindingContext<'_>, SQLError>;
}
pub type StatementAnalysisOperation<'a> =
    &'a mut dyn FnMut(&dyn StatementBindingScope) -> Result<(), SQLError>;

/// Retain a fresh catalog, namespace and transition scope for each statement analysis.
pub trait StatementAnalysisScopes {
    fn with_scope(&self, analyze: StatementAnalysisOperation<'_>) -> Result<(), SQLError>;
}
pub struct StatementAnalysisContext<'a> {
    pub scopes: &'a dyn StatementAnalysisScopes,
    pub routines: &'a dyn RoutineResolution,
}

pub fn analyze_executable_plan(
    context: &StatementAnalysisContext<'_>,
    plan: &UnifiedPlan,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    context.scopes.with_scope(&mut |scope| {
        match plan {
            UnifiedPlan::Query(query) => {
                super::analyze_query_plan_schema(
                    context.routines,
                    query,
                    params,
                    &scope.binding_context()?,
                    None,
                )?;
            }
            UnifiedPlan::Command(command) => match command.as_ref() {
                CommandPlan::Explain { body, .. } => {
                    analyze_executable_plan(context, body, params)?;
                }
                CommandPlan::CreateTableAs { query, .. }
                | CommandPlan::CreateMaterializedView { query, .. }
                | CommandPlan::DeclareCursor { query, .. } => {
                    super::analyze_query_plan_schema(
                        context.routines,
                        query,
                        params,
                        &scope.binding_context()?,
                        None,
                    )?;
                }
                _ => {
                    if command.mutation_target().is_some() {
                        analyze_command_parameters(context.routines, command, params, scope)?;
                    }
                    super::analyze_prepared_command_schema(
                        context.routines,
                        command,
                        params,
                        &scope.binding_context()?,
                    )?;
                }
            },
        }
        Ok(())
    })
}

pub fn analyze_command_parameters(
    routines: &dyn RoutineResolution,
    command: &CommandPlan,
    params: &[SQLParam],
    scope: &dyn StatementBindingScope,
) -> Result<(), SQLError> {
    let schema = RowSchema::default();
    let declared = (1..=params.len())
        .map(|index| match &params[index - 1] {
            SQLParam::Scalar(Value::Str(_) | Value::Null) => Ok(None),
            _ => crate::scalar_type(&ScalarExpr::Param(index), &schema, params),
        })
        .collect::<Result<Vec<_>, _>>()?;
    super::infer_prepared_parameter_types(
        routines,
        &UnifiedPlan::Command(Box::new(command.clone())),
        &declared,
        &scope.binding_context()?,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
