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
}

#[cfg(test)]
mod tests;
