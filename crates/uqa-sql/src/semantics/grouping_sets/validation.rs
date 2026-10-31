//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate group inputs before constant folding or row production can erase their analyzed identity.

use super::{expression_identity_with_control, resolve_grouping_expression_reference_with_control};
use crate::plan::QueryBlockPlan;
use crate::routines::RoutineResolution;
use crate::semantics::aggregates::{contains_aggregate, has_aggregate, is_aggregate};
use crate::{RowSchema, SQLError, SQLParam, ScalarExpr};
use uqa_core::memory::{ProductionControl, ProductionVec};

/// Validate the grouped expressions of a query whose own columns `schema` holds over `outer`, the scope of the queries that enclose it.
pub fn validate_grouped_expressions(
    routines: &dyn RoutineResolution,
    statement: &QueryBlockPlan,
    schema: &RowSchema,
    outer: Option<&RowSchema>,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    validate_grouped_expressions_with_inputs(
        routines,
        statement,
        schema,
        outer,
        params,
        statement
            .group_by
            .iter()
            .chain(statement.grouping_sets.iter().flatten()),
        &ProductionControl::uncontrolled(),
    )
}

/// Validate already selected procedural grouping inputs without replacing or copying the query block.
pub(crate) fn validate_grouped_expressions_with_inputs<'a>(
    routines: &dyn RoutineResolution,
    statement: &QueryBlockPlan,
    schema: &RowSchema,
    outer: Option<&RowSchema>,
    params: &[SQLParam],
    grouping: impl Iterator<Item = &'a ScalarExpr>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    let aggregates = |name: &str| routines.has_registered_aggregate_function(name);
    if statement.group_by.is_empty()
        && statement.grouping_sets.is_empty()
        && statement.having.is_none()
        && !has_aggregate(&aggregates, &statement.projections)
        && !statement
            .windows
            .iter()
            .flat_map(|window| window.spec.expressions())
            .any(|expression| contains_aggregate(&aggregates, expression))
    {
        return Ok(());
    }
    let mut groups = ProductionVec::new(*control);
    for expression in grouping {
        let expression = resolve_grouping_expression_reference_with_control(
            routines,
            expression,
            &statement.projections,
            schema,
            outer,
            params,
            control,
        )?;
        let identity =
            expression_identity_with_control(routines, expression, schema, params, control)?;
        groups.push_produced(control.finish(identity, control.empty_reservation())?)?;
    }
    let validate = |expression: &ScalarExpr| -> Result<(), SQLError> {
        expression.try_visit(&mut |part| {
            control.check()?;
            if is_aggregate(&aggregates, part) {
                return Ok(false);
            }
            let identity = expression_identity_with_control(routines, part, schema, params, control)?;
            if groups.iter().any(|group| group.as_slice() == identity.as_slice()) {
                return Ok(false);
            }
            let position = match part {
                ScalarExpr::Column(column) => schema.unqualified_position(column),
                ScalarExpr::QualifiedColumn { qualifier, column } => schema.qualified_position(qualifier, column),
                ScalarExpr::Position(position) => Some(*position),
                _ => None,
            };
            let own_width = schema.physical_width().saturating_sub(outer.map_or(0, RowSchema::physical_width));
            if position.and_then(|position| schema.physical_slot(position)).is_some_and(|slot| slot >= own_width) {
                return Ok(false);
            }
            if let Some(identity) = position.and_then(|position| schema.identity(position)) {
                let name = control.format(format_args!("{}{}{}", identity.qualifier().unwrap_or_default(), if identity.qualifier().is_some() { "." } else { "" }, identity.column()))?;
                return Err(SQLError::Routine {
                    sqlstate: "42803".into(),
                    message: control.format(format_args!("column \"{}\" must appear in the GROUP BY clause or be used in an aggregate function", name.as_str()))?.into_parts().0,
                });
            }
            Ok(true)
        })
    };
    for projection in &statement.projections {
        validate_projection(projection, schema, control, &validate)?;
    }
    for expression in statement
        .having
        .iter()
        .map(|expression| (expression, false))
        .chain(statement.order_by.iter().map(|order| (&order.expr, true)))
        .chain(
            statement
                .windows
                .iter()
                .flat_map(|window| window.spec.expressions())
                .map(|expression| (expression, false)),
        )
        .chain(
            statement
                .distinct_on
                .iter()
                .map(|expression| (expression, true)),
        )
    {
        let (expression, output_names) = expression;
        if output_names {
            if let ScalarExpr::Column(name) = expression {
                if is_output_name(statement, schema, name, control)? {
                    continue;
                }
            }
        }
        validate(expression)?;
    }
    Ok(())
}

fn validate_projection(
    projection: &crate::plan::ProjectionPlan,
    schema: &RowSchema,
    control: &ProductionControl<'_>,
    validate: &impl Fn(&ScalarExpr) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    if !crate::semantics::visit_bound_projection_star_fields(
        &projection.expr,
        schema,
        schema,
        control,
        &mut |column, position, qualifier| {
            let expression = if let Some(position) = position {
                crate::semantics::bound_projection_expression_with_control(
                    schema, position, control,
                )?
            } else {
                crate::plan::OwnedScalarExpr::column(
                    control.copy_text(column)?,
                    qualifier
                        .map(|qualifier| control.copy_text(qualifier))
                        .transpose()?,
                    control,
                )?
            };
            validate(&expression)
        },
    )? {
        validate(&projection.expr)?;
    }
    Ok(())
}

fn is_output_name(
    statement: &QueryBlockPlan,
    schema: &RowSchema,
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    let mut found = false;
    for projection in &statement.projections {
        let star = crate::semantics::visit_bound_projection_star_fields(
            &projection.expr,
            schema,
            schema,
            control,
            &mut |column, _, _| {
                found |= column == name;
                Ok(())
            },
        )?;
        found |= !star
            && crate::semantics::with_projection_label(projection, control, |label| {
                Ok(label == name)
            })?;
    }
    Ok(found)
}
