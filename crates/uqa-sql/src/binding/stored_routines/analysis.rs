//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain a fresh catalog namespace while binding stored statement and expression routines.

use super::{BoundStatementRoutines, CatalogRoutineContext};
use crate::{
    ast::ColumnType,
    binding::statements::{StatementAnalysisOperation, StatementBindingScope},
    plan::{ExpressionPlan, UnifiedPlan},
    routines::RoutineResolution,
    RowSchema, SQLError, SQLParam,
};

/// Catalog binding uses restored relation metadata and bound names without a current-routine row overlay.
pub trait CatalogRoutineScopes {
    fn with_catalog_scope(&self, analyze: StatementAnalysisOperation<'_>) -> Result<(), SQLError>;
}
#[derive(Clone, Copy)]
pub struct CatalogRoutineAnalysisContext<'a> {
    pub scopes: &'a dyn CatalogRoutineScopes,
    pub routines: &'a dyn RoutineResolution,
}
impl CatalogRoutineAnalysisContext<'_> {
    fn with_scope_result<T>(
        &self,
        mut analyze: impl FnMut(&dyn StatementBindingScope) -> Result<T, SQLError>,
    ) -> Result<T, SQLError> {
        let mut result = None;
        self.scopes.with_catalog_scope(&mut |scope| {
            result = Some(analyze(scope)?);
            Ok(())
        })?;
        result.ok_or_else(|| {
            SQLError::Internal("catalog routine scope did not invoke analysis".into())
        })
    }
    pub fn bind_statement(&self, plan: &UnifiedPlan) -> Result<BoundStatementRoutines, SQLError> {
        self.with_scope_result(|scope| {
            let binding = scope.binding_context()?;
            super::bind_catalog_statement_routines(
                &CatalogRoutineContext {
                    routines: self.routines,
                    binding: &binding,
                },
                plan,
                &[],
            )
        })
    }
    pub fn bind_rule_inputs(
        &self,
        expression: &mut ExpressionPlan,
        binding: &crate::catalog::events::RuleConditionBinding,
        table: &str,
        columns: &[crate::ast::ColumnDef],
    ) -> Result<(), SQLError> {
        let schema = binding.row_schema(columns);
        self.with_scope_result(|scope| {
            crate::binding::rule_inputs::expand(
                self.routines,
                &scope.binding_context()?,
                expression,
                binding,
                table,
                &schema,
            )
        })
    }
    pub fn bind_rule_inputs_owned(
        &self,
        expression: crate::plan::MutableExpressionPlan,
        binding: &crate::catalog::events::RuleConditionBinding,
        table: &str,
        columns: &[crate::ast::ColumnDef],
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<crate::plan::MutableExpressionPlan, SQLError> {
        let mut expression = Some(expression);
        self.with_scope_result(|scope| {
            crate::binding::preparation::bind_rule_inputs_owned(
                self.routines,
                expression.take().ok_or_else(|| {
                    SQLError::Internal("catalog scope repeated a consuming analysis".into())
                })?,
                scope.binding_context_with_control(control)?,
                binding,
                table,
                columns,
                control,
            )
        })
    }

    pub fn bind_rule_condition(
        &self,
        expression: &mut ExpressionPlan,
        outer: &RowSchema,
        event: crate::ast::RuleEvent,
    ) -> Result<Option<ColumnType>, SQLError> {
        self.with_scope_result(|scope| {
            crate::binding::routine_binding::bind_rule_condition_routines(
                self.routines,
                expression,
                &scope.binding_context()?,
                outer,
                event,
            )
        })
    }

    pub fn bind_expression(
        &self,
        expression: &mut ExpressionPlan,
        params: &[SQLParam],
        outer: &RowSchema,
    ) -> Result<Option<ColumnType>, SQLError> {
        self.with_scope_result(|scope| {
            crate::binding::bind_expression_plan_routines_for_storage(
                self.routines,
                expression,
                params,
                &scope.binding_context()?,
                outer,
            )
        })
    }

    /// Record each call's input collation in a newly defined expression whose routines this binder bound.
    pub fn assign_input_collations(
        &self,
        expression: &mut ExpressionPlan,
        outer: &RowSchema,
    ) -> Result<(), SQLError> {
        self.with_scope_result(|scope| {
            crate::binding::assign_stored_input_collations(
                self.routines,
                expression,
                outer,
                &scope.binding_context()?,
            )
        })
    }
}

#[cfg(test)]
mod tests;
