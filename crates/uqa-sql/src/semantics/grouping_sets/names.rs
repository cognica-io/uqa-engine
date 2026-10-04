//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! GROUP BY input-column precedence and output-name fallback after source binding.

use std::borrow::Cow;

use crate::plan::{ProjectionPlan, QueryBlockPlan};
use crate::routines::RoutineResolution;
use crate::{RowSchema, SQLError, SQLParam, ScalarExpr};

/// An unqualified input column wins even when its lookup is ambiguous. Output names are considered only for a whole bare name, never inside an expression. `schema` holds the query's own columns over `outer`, the scope of the queries that enclose it, whose columns do not take a name before an output column.
pub fn resolve_grouping_expression<'a>(
    routines: &dyn RoutineResolution,
    expression: &'a ScalarExpr,
    projections: &'a [ProjectionPlan],
    schema: &RowSchema,
    outer: Option<&RowSchema>,
    params: &[SQLParam],
) -> Result<Cow<'a, ScalarExpr>, SQLError> {
    let mut resolved = expression;
    if let ScalarExpr::Column(name) = expression {
        if !is_input_column(schema, outer, name) {
            let mut matches = projections
                .iter()
                .filter(|projection| crate::semantics::projection_label_at(projection) == *name);
            if let Some(first) = matches.next() {
                resolved = &first.expr;
                let mut identity = None;
                for candidate in matches {
                    let first_identity = match &identity {
                        Some(identity) => identity,
                        None => identity.insert(super::expression_identity(
                            routines, resolved, schema, params,
                        )?),
                    };
                    if *first_identity
                        != super::expression_identity(routines, &candidate.expr, schema, params)?
                    {
                        return Err(SQLError::Routine {
                            sqlstate: "42702".into(),
                            message: format!("GROUP BY \"{name}\" is ambiguous"),
                        });
                    }
                }
            }
        }
    }
    if crate::semantics::aggregates::contains_aggregate(
        &|name: &str| routines.has_registered_aggregate_function(name),
        resolved,
    ) {
        return Err(SQLError::Routine {
            sqlstate: "42803".into(),
            message: "aggregate functions are not allowed in GROUP BY".into(),
        });
    }
    if resolved.contains_window() {
        return Err(SQLError::Routine {
            sqlstate: "42P20".into(),
            message: "window functions are not allowed in GROUP BY".into(),
        });
    }
    Ok(if std::ptr::eq(resolved, expression) {
        Cow::Borrowed(expression)
    } else {
        Cow::Owned(resolved.clone())
    })
}

/// Whether `name` is a column of the query's own sources, ambiguous or not, in `schema`, which holds those columns over `outer`: `findTargetlistEntrySQL92` looks for a GROUP BY name among the columns of the query's own level only.
fn is_input_column(schema: &RowSchema, outer: Option<&RowSchema>, name: &str) -> bool {
    if !schema.has_unqualified_column(name) {
        return false;
    }
    let Some(outer) = outer else {
        return true;
    };
    let own_width = schema
        .physical_width()
        .saturating_sub(outer.physical_width());
    schema.column_slot(name).is_none_or(|slot| slot < own_width)
}

/// Bind grouping expressions before storing a query or executing its aggregation. Returns whether an output name was replaced.
pub fn bind_grouping_names(
    routines: &dyn RoutineResolution,
    statement: &mut QueryBlockPlan,
    schema: &RowSchema,
    outer: Option<&RowSchema>,
    params: &[SQLParam],
) -> Result<bool, SQLError> {
    let mut changed = false;
    for expression in statement
        .group_by
        .iter_mut()
        .chain(statement.grouping_sets.iter_mut().flatten())
    {
        if let Cow::Owned(resolved) = resolve_grouping_expression(
            routines,
            expression,
            &statement.projections,
            schema,
            outer,
            params,
        )? {
            *expression = resolved;
            changed = true;
        }
    }
    Ok(changed)
}
