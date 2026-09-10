//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relational filtering over scoped physical expression services.

use super::RelationalContext;
use crate::query::CteScope;
use crate::{Filter, PhysicalOperator, ScalarExpr, SharedExpressionEvaluator};
use std::sync::Arc;
use uqa_sql::{SQLError, SQLParam};

pub fn attach_relational_filter<'a, S: Clone + 'static>(
    context: RelationalContext<'a, S>,
    mut operator: Box<dyn PhysicalOperator + 'a>,
    predicate: Option<ScalarExpr>,
    params: &'a [SQLParam],
    ctes: &CteScope<S>,
    evaluator: &SharedExpressionEvaluator<'a>,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    if let Some(predicate) = predicate {
        operator = match context
            .expressions
            .prepare_predicate(&predicate, params, ctes)?
        {
            Some(prepared) => Box::new(Filter::with_row_predicate(operator, prepared)),
            None => Box::new(Filter::with_evaluator(
                operator,
                predicate,
                Arc::clone(evaluator),
            )),
        };
    }
    Ok(operator)
}
