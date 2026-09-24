//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate group inputs before constant folding or row production can erase their analyzed identity.

use super::{expression_identity, resolve_grouping_expression};
use crate::plan::QueryBlockPlan;
use crate::routines::RoutineResolution;
use crate::semantics::aggregates::{has_aggregate, is_aggregate};
use crate::{RowSchema, SQLError, SQLParam, ScalarExpr};

pub fn validate_grouped_expressions(
    routines: &dyn RoutineResolution,
    statement: &QueryBlockPlan,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    let aggregates = |name: &str| routines.has_registered_aggregate_function(name);
    if statement.group_by.is_empty()
        && statement.grouping_sets.is_empty()
        && statement.having.is_none()
        && !has_aggregate(&aggregates, &statement.projections)
    {
        return Ok(());
    }
    let groups = statement
        .group_by
        .iter()
        .chain(statement.grouping_sets.iter().flatten())
        .map(|expression| {
            let expression = resolve_grouping_expression(
                routines,
                expression,
                &statement.projections,
                schema,
                params,
            )?;
            expression_identity(routines, &expression, schema, params)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let projections =
        crate::semantics::expand_bound_projection_stars(&statement.projections, schema)?;
    for expression in projections
        .iter()
        .map(|projection| (&projection.expr, false))
        .chain(
            statement
                .having
                .iter()
                .map(|expression| (expression, false)),
        )
        .chain(statement.order_by.iter().map(|order| (&order.expr, true)))
        .chain(
            statement
                .distinct_on
                .iter()
                .map(|expression| (expression, true)),
        )
    {
        let (expression, output_names) = expression;
        // Ordering and DISTINCT ON may select an already checked output expression by name.
        if output_names
            && matches!(expression, ScalarExpr::Column(name) if projections.iter().any(|projection| crate::semantics::projection_label_at(projection) == *name))
        {
            continue;
        }
        expression.try_visit(&mut |part| {
            if is_aggregate(&aggregates, part)
                || groups.contains(&expression_identity(routines, part, schema, params)?)
            {
                return Ok(false);
            }
            let position = match part {
                ScalarExpr::Column(column) => schema.unqualified_position(column),
                ScalarExpr::QualifiedColumn { qualifier, column } => {
                    schema.qualified_position(qualifier, column)
                }
                ScalarExpr::Position(position) => Some(*position),
                _ => None,
            };
            if let Some(identity) = position.and_then(|position| schema.identity(position)) {
                let name = identity.qualifier().map_or_else(
                    || identity.column().to_owned(),
                    |qualifier| format!("{qualifier}.{}", identity.column()),
                );
                return Err(SQLError::Routine {
                    sqlstate: "42803".into(),
                    message: format!("column \"{name}\" must appear in the GROUP BY clause or be used in an aggregate function"),
                });
            }
            Ok(true)
        })?;
    }
    Ok(())
}
