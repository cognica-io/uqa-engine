//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Dependencies of FROM inputs before their output namespace becomes visible.

use super::outputs::{admits_preceding, source_scope_in_namespace};
use super::{
    enclosing_scope, expression_has_external_reference, query_references_outer, CorrelationContext,
    QueryPlan, QueryScope, SQLError, ScalarExpr, SourcePlan,
};

use super::memory::{copy_scopes, extend_scopes, Analysis, Ctes};
use crate::production::Buffer;
use uqa_core::memory::ProductionControl;

/// Whether a FROM item needs the calling join's current row. An explicit LATERAL marker permits references but does not itself require per-row execution. Enclosing-query identities in `input` remain constant during this join.
pub fn source_depends_on_current_row(
    context: CorrelationContext<'_>,
    source: &SourcePlan,
    subqueries: &[QueryPlan],
    input: &crate::RowSchema,
) -> Result<bool, SQLError> {
    let control = ProductionControl::uncontrolled();
    let mut scopes = Buffer::new(&control);
    scopes.push(enclosing_scope(input, &control)?, &control)?;
    source_references_outer(
        Analysis { context, control },
        source,
        subqueries,
        &mut scopes,
        &Ctes::new(),
    )
}

pub(super) fn source_references_outer(
    context: Analysis<'_, '_>,
    source: &SourcePlan,
    subqueries: &[QueryPlan],
    scopes: &mut Buffer<QueryScope>,
    ctes: &Ctes,
) -> Result<bool, SQLError> {
    source_references_outer_in_scope(context, source, subqueries, scopes, ctes, &[], false)
}

fn source_references_outer_in_scope(
    context: Analysis<'_, '_>,
    source: &SourcePlan,
    subqueries: &[QueryPlan],
    scopes: &mut Buffer<QueryScope>,
    ctes: &Ctes,
    preceding: &[QueryScope],
    inherited_lateral: bool,
) -> Result<bool, SQLError> {
    context.control.check()?;
    let length = scopes.len();
    if admits_preceding(source, inherited_lateral) {
        extend_scopes(scopes, preceding, &context.control)?;
    }
    let result = (|| match source {
        SourcePlan::RetainedRows(_) | SourcePlan::Table { .. } => Ok(false),
        SourcePlan::Join {
            left,
            right,
            on,
            lateral,
            ..
        } => {
            if source_references_outer_in_scope(
                context, left, subqueries, scopes, ctes, preceding, false,
            )? {
                return Ok(true);
            }
            let left_scope =
                source_scope_in_namespace(context, left, ctes, scopes, preceding, false)?;
            let mut right_preceding = copy_scopes(preceding, &context.control)?;
            right_preceding.push(
                left_scope.copy_with_control(&context.control)?,
                &context.control,
            )?;
            if source_references_outer_in_scope(
                context,
                right,
                subqueries,
                scopes,
                ctes,
                &right_preceding,
                *lateral,
            )? {
                return Ok(true);
            }
            let Some(on) = on else {
                return Ok(false);
            };
            let right_scope = source_scope_in_namespace(
                context,
                right,
                ctes,
                scopes,
                &right_preceding,
                *lateral,
            )?;
            scopes.push(left_scope, &context.control)?;
            scopes.push(right_scope, &context.control)?;
            expression_references_outer(context, on, subqueries, scopes, ctes)
        }
        SourcePlan::Values { rows, .. } => {
            for expression in rows.iter().flatten() {
                if expression_references_outer(context, expression, subqueries, scopes, ctes)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        SourcePlan::Function { args, .. } => {
            arguments_reference_outer(context, args, subqueries, scopes, ctes)
        }
        SourcePlan::FunctionGroup { functions, .. } => {
            for function in functions {
                if arguments_reference_outer(context, &function.args, subqueries, scopes, ctes)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        SourcePlan::Subquery { body, .. } => query_references_outer(context, body, scopes, ctes),
    })();
    while scopes.len() > length {
        scopes.pop();
    }
    result
}

fn arguments_reference_outer(
    context: Analysis<'_, '_>,
    args: &[ScalarExpr],
    subqueries: &[QueryPlan],
    scopes: &mut Buffer<QueryScope>,
    ctes: &Ctes,
) -> Result<bool, SQLError> {
    for argument in args {
        context.control.check()?;
        if expression_references_outer(context, argument, subqueries, scopes, ctes)? {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn expression_references_outer(
    context: Analysis<'_, '_>,
    expression: &ScalarExpr,
    subqueries: &[QueryPlan],
    scopes: &mut Buffer<QueryScope>,
    ctes: &Ctes,
) -> Result<bool, SQLError> {
    if expression_has_external_reference(expression, scopes, &context.control)? {
        return Ok(true);
    }
    let mut dependent = false;
    expression.try_visit(&mut |node| {
        context.control.check()?;
        if dependent {
            return Ok(false);
        }
        let index = match node {
            ScalarExpr::ScalarSubquery(index)
            | ScalarExpr::Exists {
                subquery: index, ..
            }
            | ScalarExpr::InSubquery {
                subquery: index, ..
            } => *index,
            _ => return Ok(true),
        };
        let query = subqueries.get(index).ok_or_else(|| {
            SQLError::Internal(format!(
                "missing scalar subquery {index} during correlation analysis"
            ))
        })?;
        dependent = query_references_outer(context, query, scopes, ctes)?;
        Ok::<_, SQLError>(!dependent)
    })?;
    Ok(dependent)
}
