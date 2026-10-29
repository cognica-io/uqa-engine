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

/// A nested command retains its enclosing CTE bindings while the session supplies fresh statement metadata and admission.
pub struct InheritedStatementAnalysisScopes<'a> {
    pub current: &'a dyn StatementAnalysisScopes,
    pub parent: &'a dyn StatementBindingScope,
}

impl StatementAnalysisScopes for InheritedStatementAnalysisScopes<'_> {
    fn with_scope(&self, analyze: StatementAnalysisOperation<'_>) -> Result<(), SQLError> {
        self.current.with_scope(&mut |scope| {
            let mut binding = super::snapshot::BindingSnapshot::from(scope.binding_context()?);
            let parent = super::snapshot::BindingSnapshot::from(self.parent.binding_context()?);
            binding.inherit_cte_bindings(&parent);
            analyze(&binding)
        })
    }
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
    analyze_executable_plan_for_cache(context, plan, params).map(|(result, _)| result)
}

mod procedural;
pub use procedural::{analyze_procedural_plan, ProceduralPlanAnalysis};
mod cache;
pub use cache::{analyze_for_statement_reuse, AnalyzedStatement};

/// Analyze once and report whether the converted inputs can be reused by a later
/// ordinary message. Prepared definitions use their separate creation lifetime.
pub fn analyze_executable_plan_for_cache(
    context: &StatementAnalysisContext<'_>,
    plan: &mut UnifiedPlan,
    params: &[SQLParam],
) -> Result<(AnalyzedResult, bool), SQLError> {
    analyze_reusable_inputs(context, plan, params)
        .map(|(result, reusable)| (result, reusable && params.is_empty()))
}

/// Analyze in the selected scope and report whether immutable input conversions can survive ordinary messages. Callers capture optimizer resources only after successful analysis, then retain reusable syntax through `AnalyzedStatement`.
pub fn analyze_reusable_inputs(
    context: &StatementAnalysisContext<'_>,
    plan: &mut UnifiedPlan,
    params: &[SQLParam],
) -> Result<(AnalyzedResult, bool), SQLError> {
    let mut result = None;
    let mut reusable = true;
    context.scopes.with_scope(&mut |scope| {
        result = Some(match plan {
            UnifiedPlan::Command(command) => match command.as_mut() {
                // The explained statement retains a scope of its own.
                CommandPlan::Explain { .. } => {
                    let mut explained = None;
                    context.scopes.with_scope(&mut |scope| {
                        explained = Some(analyze_plan_result_inner(
                            context.routines,
                            context.aliases,
                            plan,
                            params,
                            scope,
                            &mut reusable,
                        )?);
                        Ok(())
                    })?;
                    explained.ok_or_else(|| {
                        SQLError::Internal("EXPLAIN analysis scope did not run".into())
                    })?
                }
                _ => analyze_plan_result_inner(
                    context.routines,
                    context.aliases,
                    plan,
                    params,
                    scope,
                    &mut reusable,
                )?,
            },
            UnifiedPlan::Query(_) => analyze_plan_result_inner(
                context.routines,
                context.aliases,
                plan,
                params,
                scope,
                &mut reusable,
            )?,
        });
        Ok(())
    })?;
    result
        .map(|result| (result, reusable))
        .ok_or_else(|| SQLError::Internal("statement analysis scope did not run".into()))
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
    analyze_plan_result_inner(routines, aliases, plan, params, scope, &mut true)
}

fn analyze_plan_result_inner(
    routines: &dyn RoutineResolution,
    aliases: &dyn crate::schema::dependencies::oid_alias::OidAliasInput,
    plan: &mut UnifiedPlan,
    params: &[SQLParam],
    scope: &dyn StatementBindingScope,
    reusable: &mut bool,
) -> Result<AnalyzedResult, SQLError> {
    if let UnifiedPlan::Command(command) = plan {
        if let CommandPlan::Explain {
            body,
            options,
            unbound_parameters,
        } = command.as_mut()
        {
            if params.is_empty() && options.infers_unbound_parameters()? {
                let (parameters, body_reusable) = super::preparation::read_generic_explain_inputs(
                    routines,
                    body,
                    &scope.binding_context()?,
                    aliases,
                )?;
                *unbound_parameters = Some(parameters);
                *reusable &= body_reusable;
            } else {
                *unbound_parameters = None;
                analyze_plan_result_inner(routines, aliases, body, params, scope, reusable)?;
            }
            return options.result_schema().map(AnalyzedResult::Schema);
        }
    }
    let binding = scope.binding_context()?;
    let analysis =
        super::preparation::read_executable_inputs(routines, plan, params, &binding, aliases)?;
    *reusable &= analysis.reusable;
    Ok(analysis.result)
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
