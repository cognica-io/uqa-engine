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
    pub aliases: &'a dyn crate::schema::dependencies::oid_alias::OidAliasInput,
}

/// The result a statement's analysis derives: the column types of a query or of a data-modifying statement's `RETURNING` list, `None` where analysis derives no type, or no result for any other statement.
#[derive(Debug, Clone, PartialEq)]
pub enum AnalyzedResult {
    Rows(Vec<Option<crate::ColumnType>>),
    /// An analyzed output whose anonymous records retain their field descriptors.
    Schema(RowSchema),
    Command,
}

impl AnalyzedResult {
    pub fn column_types(&self) -> Option<&[Option<crate::ColumnType>]> {
        match self {
            Self::Rows(types) => Some(types),
            Self::Schema(schema) => Some(schema.column_types()),
            Self::Command => None,
        }
    }

    pub fn record_fields(&self, column: usize) -> Option<&crate::schema::RecordFields> {
        match self {
            Self::Schema(schema) => schema.record_fields(column),
            _ => None,
        }
    }
}

impl StatementBindingScope for super::snapshot::BindingSnapshot {
    fn binding_context(&self) -> Result<BindingContext<'_>, SQLError> {
        Ok(self.context())
    }
}

pub fn analyze_executable_plan(
    context: &StatementAnalysisContext<'_>,
    plan: &mut UnifiedPlan,
    params: &[SQLParam],
) -> Result<AnalyzedResult, SQLError> {
    let mut result = None;
    context.scopes.with_scope(&mut |scope| {
        result = Some(match plan {
            UnifiedPlan::Command(command) => match command.as_mut() {
                // The explained statement retains a scope of its own.
                CommandPlan::Explain { body, .. } => {
                    analyze_executable_plan(context, body, params)?;
                    AnalyzedResult::Command
                }
                _ => analyze_plan_result(context.routines, context.aliases, plan, params, scope)?,
            },
            UnifiedPlan::Query(_) => {
                analyze_plan_result(context.routines, context.aliases, plan, params, scope)?
            }
        });
        Ok(())
    })?;
    result.ok_or_else(|| SQLError::Internal("statement analysis scope did not run".into()))
}

/// Analyze a statement in one binding scope, retain input-function results in its
/// executable tree and derive its result without running statement expressions.
pub fn analyze_plan_result(
    routines: &dyn RoutineResolution,
    aliases: &dyn crate::schema::dependencies::oid_alias::OidAliasInput,
    plan: &mut UnifiedPlan,
    params: &[SQLParam],
    scope: &dyn StatementBindingScope,
) -> Result<AnalyzedResult, SQLError> {
    let binding = scope.binding_context()?;
    super::preparation::read_executable_inputs(routines, plan, params, &binding, aliases)?;
    let rows = AnalyzedResult::Schema;
    match plan {
        UnifiedPlan::Query(query) => {
            super::analyze_query_plan_schema(routines, query, params, &binding, None).map(rows)
        }
        UnifiedPlan::Command(command) => match command.as_mut() {
            CommandPlan::Explain { body, .. } => {
                analyze_plan_result(routines, aliases, body, params, scope)?;
                Ok(AnalyzedResult::Command)
            }
            CommandPlan::CreateView { query, .. }
            | CommandPlan::CreateTableAs { query, .. }
            | CommandPlan::CreateMaterializedView { query, .. }
            | CommandPlan::DeclareCursor { query, .. } => {
                super::analyze_query_plan_schema(routines, query, params, &binding, None)?;
                Ok(AnalyzedResult::Command)
            }
            _ => Ok(
                super::analyze_prepared_command_schema(routines, command, params, &binding)?
                    .map_or(AnalyzedResult::Command, rows),
            ),
        },
    }
}

pub fn analyze_command_parameters(
    routines: &dyn RoutineResolution,
    command: &CommandPlan,
    params: &[SQLParam],
    scope: &dyn StatementBindingScope,
) -> Result<(), SQLError> {
    let declared = parameter_input_types(params)?;
    super::infer_prepared_parameter_types(
        routines,
        &UnifiedPlan::Command(Box::new(command.clone())),
        &declared,
        &scope.binding_context()?,
    )?;
    Ok(())
}

pub(super) fn parameter_input_types(
    params: &[SQLParam],
) -> Result<Vec<Option<crate::ColumnType>>, SQLError> {
    let schema = RowSchema::default();
    (1..=params.len())
        .map(|index| match &params[index - 1] {
            SQLParam::Scalar(Value::Str(_) | Value::Null) => Ok(None),
            _ => crate::scalar_type(&ScalarExpr::Param(index), &schema, params),
        })
        .collect()
}

#[cfg(test)]
mod tests;
