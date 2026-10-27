//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relational filtering over scoped physical expression services.

use super::RelationalContext;
use crate::physical::construction::OperatorBuild;
use crate::query::CteScope;
use crate::{Filter, PhysicalOperator, ScalarExpr, SharedExpressionEvaluator};
use std::sync::Arc;
use uqa_sql::{SQLError, SQLParam};

pub fn attach_relational_filter<'a, S: Clone + 'static>(
    context: RelationalContext<'a, S>,
    operator: Box<dyn PhysicalOperator + 'a>,
    predicate: Option<ScalarExpr>,
    params: &'a [SQLParam],
    ctes: &CteScope<S>,
    evaluator: &SharedExpressionEvaluator<'a>,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    OperatorBuild::build(operator, |operator| {
        if let Some(predicate) = predicate {
            let prepared = context
                .expressions
                .prepare_predicate(&predicate, params, ctes)?;
            operator.wrap(|input| match prepared {
                Some(prepared) => Box::new(Filter::with_row_predicate(input, prepared)),
                None => Box::new(Filter::with_evaluator(
                    input,
                    predicate,
                    Arc::clone(evaluator),
                )),
            });
        }
        Ok(())
    })
}
