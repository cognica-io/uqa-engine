//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Literal output identities propagated through derived query boundaries.

use super::{output_expressions, CteScope, RoutineResolution, SQLError, SQLParam, ScalarExpr};
use uqa_sql::plan::{QueryPlan, RelationalPlan, SourcePlan};

pub(super) fn constant_expression<S: Clone>(
    routines: &dyn RoutineResolution,
    expression: &ScalarExpr,
    source: Option<&SourcePlan>,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<bool, SQLError> {
    let (qualifier, column) = match expression {
        ScalarExpr::Literal(_) | ScalarExpr::TypedLiteral { .. } => return Ok(true),
        ScalarExpr::Column(column) => (None, column),
        ScalarExpr::QualifiedColumn { qualifier, column } => (Some(qualifier), column),
        _ => return Ok(false),
    };
    match source {
        Some(SourcePlan::Table {
            name,
            qualifier: source_qualifier,
            alias,
            column_aliases,
            ..
        }) => {
            if qualifier.is_some_and(|qualifier| {
                qualifier != source_qualifier && Some(qualifier) != alias.as_ref()
            }) {
                return Ok(false);
            }
            let Some(cte) = ctes.deferred_reference(name) else {
                return Ok(false);
            };
            let Some(query) = cte.body.query() else {
                return Ok(false);
            };
            let mut parent = ctes.clone();
            parent.remove_deferred(&cte.name);
            constant_output(
                routines,
                query,
                column,
                &cte.columns,
                column_aliases,
                params,
                &parent,
            )
        }
        Some(SourcePlan::Subquery {
            body,
            alias,
            column_aliases,
            ..
        }) => {
            if qualifier.is_some_and(|qualifier| Some(qualifier) != alias.as_ref()) {
                return Ok(false);
            }
            constant_output(routines, body, column, &[], column_aliases, params, ctes)
        }
        _ => Ok(false),
    }
}

fn constant_output<S: Clone>(
    routines: &dyn RoutineResolution,
    query: &QueryPlan,
    column: &str,
    cte_columns: &[String],
    source_columns: &[String],
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<bool, SQLError> {
    let RelationalPlan::QueryBlock(block) = &query.root else {
        return Ok(false);
    };
    let mut scope = std::borrow::Cow::Borrowed(ctes);
    for cte in uqa_sql::semantics::ordered_plan_ctes(query)? {
        scope.to_mut().insert_deferred(cte.clone());
    }
    let output = output_expressions(routines, block, params, &scope)?;
    let mut found = false;
    for (index, (label, expression)) in output.iter().enumerate() {
        if source_columns
            .get(index)
            .or_else(|| cte_columns.get(index))
            .unwrap_or(label)
            != column
        {
            continue;
        }
        found = true;
        if !constant_expression(routines, expression, block.from.as_ref(), params, &scope)? {
            return Ok(false);
        }
    }
    Ok(found)
}
