//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Probe decorrelated EXISTS keys against positional outer rows.

use super::{
    analysis, context::ScopedSubqueryHooks, execution::build_correlated_exists, SubqueryContext,
    SubqueryServices,
};
use crate::physical::physical_exec_error;
use crate::query::{
    scope::subqueries::{CachedCorrelatedExists, CorrelatedExistsOuterKeys},
    CteScope,
};
use crate::scalar::plan::{
    eval_physical_scalar, PhysicalEvalContext, PhysicalOuterRow, PhysicalSubqueryRunner,
};
use crate::{ExecResult, RowSchemaExecution, ScalarExpr};
use std::sync::Arc;
use uqa_core::Value;
use uqa_sql::{
    expr::{EngineHook, RowLookup},
    semantics::volatility::query_contains_volatile_function,
    SQLError, SQLParam,
};

#[cfg(test)]
mod tests;

impl<S: Clone + 'static> SubqueryContext<'_, S> {
    pub(super) fn correlated_exists_matches(
        &self,
        lookup: &CachedCorrelatedExists,
        outer_row: PhysicalOuterRow<'_>,
        params: &[SQLParam],
    ) -> Result<bool, SQLError> {
        correlated_exists_matches(
            self.ctes,
            self.function_hook,
            self.subquery_runner,
            lookup,
            outer_row,
            params,
        )
    }
}

fn correlated_exists_matches<S: Clone>(
    ctes: &CteScope<S>,
    function_hook: &dyn EngineHook,
    subquery_runner: &dyn PhysicalSubqueryRunner,
    lookup: &CachedCorrelatedExists,
    outer_row: PhysicalOuterRow<'_>,
    params: &[SQLParam],
) -> Result<bool, SQLError> {
    with_outer_lookup(outer_row, |outer_row| match &lookup.outer_keys {
        CorrelatedExistsOuterKeys::Direct(columns) => {
            let mut key = smallvec::SmallVec::<[&Value; 4]>::with_capacity(columns.len());
            for column in columns {
                let Some(value) = column.value(outer_row) else {
                    return Ok(false);
                };
                if matches!(value, Value::Null) {
                    return Ok(false);
                }
                key.push(value);
            }
            lookup
                .keys
                .contains_borrowed(&key)
                .map_err(physical_exec_error)
        }
        CorrelatedExistsOuterKeys::Evaluated(expressions) => {
            let context = PhysicalEvalContext::from_row_lookup(outer_row, params)
                .with_function_hook(function_hook)
                .with_subquery_runner(subquery_runner);
            let mut key = smallvec::SmallVec::<[Value; 4]>::with_capacity(expressions.len());
            for expression in expressions {
                let value = eval_physical_scalar(expression, &ctes.scalar_subqueries, &context)?;
                if matches!(value, Value::Null) {
                    return Ok(false);
                }
                key.push(value);
            }
            lookup
                .keys
                .contains_values(&key)
                .map_err(physical_exec_error)
        }
    })
}

fn with_outer_lookup<T>(
    outer_row: PhysicalOuterRow<'_>,
    evaluate: impl FnOnce(&dyn RowLookup) -> Result<T, SQLError>,
) -> Result<T, SQLError> {
    match outer_row {
        PhysicalOuterRow::Physical { schema, row } => evaluate(&schema.view(row)),
        PhysicalOuterRow::Absent => Err(SQLError::Internal(
            "correlated subquery requires an outer row".into(),
        )),
    }
}

struct PreparedCorrelatedExistsPredicate<'a, S: Clone> {
    hooks: &'a dyn ScopedSubqueryHooks<S>,
    params: &'a [SQLParam],
    ctes: CteScope<S>,
    lookup: Arc<CachedCorrelatedExists>,
    negated: bool,
}

impl<S: Clone + Send + Sync + 'static> crate::RowPredicate
    for PreparedCorrelatedExistsPredicate<'_, S>
{
    fn keep_physical(
        &self,
        schema: &crate::RowSchema,
        row: &crate::PhysicalRow,
    ) -> ExecResult<bool> {
        let exists = self
            .hooks
            .with_hooks(&self.ctes, &mut |function_hook, subquery_runner| {
                correlated_exists_matches(
                    &self.ctes,
                    function_hook,
                    subquery_runner,
                    &self.lookup,
                    PhysicalOuterRow::Physical { schema, row },
                    self.params,
                )
            })?;
        Ok(if self.negated { !exists } else { exists })
    }
}

/// Prepare a simple immutable correlated EXISTS before the outer scan starts. The filter then probes its key set directly, avoiding a scalar-expression walk and shared subquery-cache lock for every outer row.
pub fn prepare_correlated_exists_predicate<'a, S: Clone + Send + Sync + 'static>(
    services: &SubqueryServices<'a, S>,
    expression: &ScalarExpr,
    params: &'a [SQLParam],
    ctes: &CteScope<S>,
) -> Result<Option<crate::SharedRowPredicate<'a>>, SQLError> {
    let ScalarExpr::Exists { subquery, negated } = expression else {
        return Ok(None);
    };
    let Some(plan) = ctes.scalar_subqueries.get(*subquery) else {
        return Err(SQLError::Internal(format!(
            "physical scalar subquery slot {subquery} is out of bounds"
        )));
    };
    if query_contains_volatile_function(services.volatility, plan)?
        || !analysis::query_depends_on_outer_row(services, plan)?
    {
        return Ok(None);
    }
    let Some(lookup) = build_correlated_exists(services, ctes, plan, params)? else {
        return Ok(None);
    };
    Ok(Some(Arc::new(PreparedCorrelatedExistsPredicate {
        hooks: services.hooks,
        params,
        ctes: ctes.clone(),
        lookup,
        negated: *negated,
    })))
}
