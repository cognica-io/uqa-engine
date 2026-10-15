//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Predicate preparation retained only by one mutation's immutable read scope.

use super::{eval_bound_mutation_expr, MutationExpressionContext};
use crate::{
    query::{set_projection::SetFunctionRuntime, CteScope},
    OwnedPhysicalRow, RowSchema, ScalarExpr,
};
use std::sync::Arc;
use uqa_sql::{SQLError, SQLParam};

struct PreparedPredicate<'a> {
    schema: RowSchema,
    expression: ScalarExpr,
    hook: Arc<dyn SetFunctionRuntime + 'a>,
}

/// Retain one typed predicate without retaining row values or sharing mutable statement state.
pub(crate) struct MutationPredicate<'a, S: Clone + 'static> {
    services: MutationExpressionContext<'a, S>,
    ctes: &'a CteScope<S>,
    expression: Option<&'a ScalarExpr>,
    params: &'a [SQLParam],
    prepared: Option<PreparedPredicate<'a>>,
}

impl<'a, S: Clone + 'static> MutationPredicate<'a, S> {
    pub(crate) fn new(
        services: MutationExpressionContext<'a, S>,
        ctes: &'a CteScope<S>,
        expression: Option<&'a ScalarExpr>,
        params: &'a [SQLParam],
    ) -> Self {
        Self {
            services,
            ctes,
            expression,
            params,
            prepared: None,
        }
    }

    pub(crate) fn qualifies(&mut self, row: &OwnedPhysicalRow) -> Result<bool, SQLError> {
        let Some(expression) = self.expression else {
            return Ok(true);
        };
        if self
            .prepared
            .as_ref()
            .is_none_or(|prepared| prepared.schema != row.schema)
        {
            let hook = self.services.expressions.bind_scope(self.ctes.clone());
            crate::scalar_type_with_resolver(
                expression,
                &row.schema,
                self.params,
                self.services.types,
            )?;
            let expression = crate::bind_type_introspection_with_resolver(
                expression.clone(),
                &row.schema,
                self.params,
                self.services.types,
            );
            self.prepared = Some(PreparedPredicate {
                schema: row.schema.clone(),
                expression,
                hook,
            });
        }
        let prepared = self
            .prepared
            .as_ref()
            .expect("predicate prepared for this schema");
        eval_bound_mutation_expr(
            &prepared.expression,
            &self.ctes.scalar_subqueries,
            prepared.hook.as_ref(),
            Some(row),
            self.params,
            None,
        )
        .map(|value| uqa_sql::expr::truthy(&value))
    }
}
