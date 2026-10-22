//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Dependencies of FROM inputs before their output namespace becomes visible.

use super::outputs::{admits_preceding, source_scope_in_namespace};
use super::{
    enclosing_scope, expression_has_external_reference, query_references_outer, BTreeMap,
    CorrelationContext, QueryPlan, QueryScope, RelationColumns, SQLError, ScalarExpr, SourcePlan,
};

/// Whether a FROM item needs the calling join's current row. An explicit LATERAL marker permits references but does not itself require per-row execution. Enclosing-query identities in `input` remain constant during this join.
pub fn source_depends_on_current_row(
    context: CorrelationContext<'_>,
    source: &SourcePlan,
    subqueries: &[QueryPlan],
    input: &crate::RowSchema,
) -> Result<bool, SQLError> {
    source_references_outer(
        context,
        source,
        subqueries,
        &mut vec![enclosing_scope(input)],
        &BTreeMap::new(),
    )
}

pub(super) fn source_references_outer(
    context: CorrelationContext<'_>,
    source: &SourcePlan,
    subqueries: &[QueryPlan],
    scopes: &mut Vec<QueryScope>,
    ctes: &BTreeMap<String, RelationColumns>,
) -> Result<bool, SQLError> {
    source_references_outer_in_scope(context, source, subqueries, scopes, ctes, &[], false)
}

fn source_references_outer_in_scope(
    context: CorrelationContext<'_>,
    source: &SourcePlan,
    subqueries: &[QueryPlan],
    scopes: &mut Vec<QueryScope>,
    ctes: &BTreeMap<String, RelationColumns>,
    preceding: &[QueryScope],
    inherited_lateral: bool,
) -> Result<bool, SQLError> {
    let length = scopes.len();
    if admits_preceding(source, inherited_lateral) {
        scopes.extend_from_slice(preceding);
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
            let mut right_preceding = preceding.to_vec();
            right_preceding.push(left_scope.clone());
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
            scopes.push(left_scope);
            scopes.push(right_scope);
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
    scopes.truncate(length);
    result
}

fn arguments_reference_outer(
    context: CorrelationContext<'_>,
    args: &[ScalarExpr],
    subqueries: &[QueryPlan],
    scopes: &mut Vec<QueryScope>,
    ctes: &BTreeMap<String, RelationColumns>,
) -> Result<bool, SQLError> {
    for argument in args {
        if expression_references_outer(context, argument, subqueries, scopes, ctes)? {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn expression_references_outer(
    context: CorrelationContext<'_>,
    expression: &ScalarExpr,
    subqueries: &[QueryPlan],
    scopes: &mut Vec<QueryScope>,
    ctes: &BTreeMap<String, RelationColumns>,
) -> Result<bool, SQLError> {
    if expression_has_external_reference(expression, scopes) {
        return Ok(true);
    }
    let mut dependent = false;
    expression.try_visit(&mut |node| {
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
