//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relational filtering over scoped physical expression services.

use super::RelationalContext;
use crate::physical::construction::OperatorBuild;
use crate::query::CteScope;
use crate::{Filter, PhysicalOperator, SharedExpressionEvaluator};
use std::sync::Arc;
use uqa_sql::{SQLError, SQLParam};

pub fn attach_relational_filter<'a, S: Clone + 'static>(
    context: RelationalContext<'a, S>,
    operator: Box<dyn PhysicalOperator + 'a>,
    predicate: Option<uqa_sql::plan::OwnedScalarExpr>,
    params: &'a [SQLParam],
    ctes: &CteScope<S>,
    evaluator: &SharedExpressionEvaluator<'a>,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    OperatorBuild::build(operator, |operator| {
        if let Some(predicate) = predicate {
            let prepared = context
                .expressions
                .prepare_predicate(&predicate, params, ctes)?;
            let resources = context.expressions.production_resources()?;
            let control = resources.control();
            operator.attach(|input| match prepared {
                Some(prepared) => Filter::with_row_predicate(input, prepared).into_boxed(&control),
                None => {
                    Filter::with_owned_predicate(input, predicate, Arc::clone(evaluator), &control)?
                        .into_boxed(&control)
                }
            })?;
        }
        Ok(())
    })
}
