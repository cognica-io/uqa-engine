//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Dependencies of FROM inputs before their output namespace becomes visible.

use super::{
    enclosing_scope, expression_has_external_reference, query_references_outer, source_scope,
    BTreeMap, CorrelationContext, QueryPlan, QueryScope, RelationColumns, SQLError, ScalarExpr,
    SourcePlan,
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
    match source {
        SourcePlan::RetainedRows(_) | SourcePlan::Table { .. } => Ok(false),
        SourcePlan::Join {
            left,
            right,
            on,
            lateral,
            ..
        } => {
            if source_references_outer(context, left, subqueries, scopes, ctes)? {
                return Ok(true);
            }
            let sees_left = *lateral
                || matches!(
                    right.as_ref(),
                    SourcePlan::Function { .. } | SourcePlan::FunctionGroup { .. }
                );
            if sees_left {
                scopes.push(source_scope(context, left, ctes)?);
            }
            let result = source_references_outer(context, right, subqueries, scopes, ctes);
            if sees_left {
                scopes.pop();
            }
            if result? {
                return Ok(true);
            }
            let Some(on) = on else {
                return Ok(false);
            };
            let left_scope = source_scope(context, left, ctes)?;
            let right_scope = source_scope(context, right, ctes)?;
            scopes.push(left_scope);
            scopes.push(right_scope);
            let result = expression_references_outer(context, on, subqueries, scopes, ctes);
            scopes.pop();
            scopes.pop();
            result
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
    }
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
