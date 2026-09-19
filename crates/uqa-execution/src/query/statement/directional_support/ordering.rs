//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Identify projection and window operators that remain above final ordering.

use crate::query::{
    binding::bind_source_plan_schema,
    ordering::{order_projection, resolve_order_expression},
    CteScope, OutputColumnMapping,
};
use crate::ProjectionTarget;
use uqa_sql::{
    ast::NullsOrder,
    plan::QueryBlockPlan,
    routines::RoutineResolution,
    semantics::{
        aggregates::exprs_match, projection_columns,
        sets::static_setness::function_may_return_set_statically,
    },
    SQLError, SQLParam, ScalarExpr,
};

mod constants;

pub(super) fn has_effective_ordering<S: Clone>(
    routines: &dyn RoutineResolution,
    block: &QueryBlockPlan,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<bool, SQLError> {
    let output = output_expressions(routines, block, params, ctes)?;
    for order in &block.order_by {
        let expression = resolve_order_expression(&order.expr, &output)?;
        if !constants::constant_expression(
            routines,
            &expression,
            block.from.as_ref(),
            params,
            ctes,
        )? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn expression_returns_set(catalog: &dyn RoutineResolution, expression: &ScalarExpr) -> bool {
    let mut returns_set = false;
    expression.visit(&mut |expression| {
        if let ScalarExpr::Func { name, binding, .. } = expression {
            returns_set |= function_may_return_set_statically(catalog, name, binding.as_ref());
        }
    });
    returns_set
}

pub(super) fn projections_return_set(
    catalog: &dyn RoutineResolution,
    block: &QueryBlockPlan,
) -> bool {
    block
        .projections
        .iter()
        .any(|projection| expression_returns_set(catalog, &projection.expr))
}

fn output_expressions<S: Clone>(
    routines: &dyn RoutineResolution,
    block: &QueryBlockPlan,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<Vec<OutputColumnMapping>, SQLError> {
    if !block.projections.iter().any(|projection| {
        matches!(
            projection.expr,
            ScalarExpr::Star | ScalarExpr::QualifiedStar(_)
        )
    }) {
        return Ok(projection_columns(&block.projections)
            .into_iter()
            .zip(
                block
                    .projections
                    .iter()
                    .map(|projection| projection.expr.clone()),
            )
            .collect());
    }
    let schema = block.from.as_ref().map_or_else(
        || Ok(crate::RowSchema::default()),
        |source| bind_source_plan_schema(routines, source, params, ctes, None),
    )?;
    let (projections, mut output) = order_projection(&block.projections, &schema)?;
    for (_, expression) in &mut output {
        match expression {
            ScalarExpr::InternalColumn(column) => {
                if let Some((_, projected)) = projections
                    .iter()
                    .find(|(target, _)| *target == ProjectionTarget::Internal(*column))
                {
                    *expression = projected.clone();
                }
            }
            ScalarExpr::Position(position) => {
                *expression = ScalarExpr::Column(schema.columns()[*position].clone());
            }
            _ => {}
        }
    }
    Ok(output)
}

pub(super) fn ordered_set_projection<S: Clone>(
    routines: &dyn RoutineResolution,
    block: &QueryBlockPlan,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<bool, SQLError> {
    let output = output_expressions(routines, block, params, ctes)?;
    for order in &block.order_by {
        if expression_returns_set(routines, &resolve_order_expression(&order.expr, &output)?) {
            // All target SRFs stay at the same level when any is a sorting key.
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn window_preserves_order<S: Clone>(
    routines: &dyn RoutineResolution,
    block: &QueryBlockPlan,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<bool, SQLError> {
    let output = output_expressions(routines, block, params, ctes)?;
    let required = block
        .order_by
        .iter()
        .map(|order| {
            Ok((
                resolve_order_expression(&order.expr, &output)?,
                order.descending,
                order.nulls,
            ))
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    let mut preserves = false;
    for projection in &block.projections {
        projection.expr.visit(&mut |expression| {
            let ScalarExpr::WindowCall { spec, .. } = expression else {
                return;
            };
            let window_order = spec
                .partition_by
                .iter()
                .map(|expr| (expr, false, None))
                .chain(
                    spec.order_by
                        .iter()
                        .map(|order| (&order.expr, order.descending, order.nulls)),
                )
                .collect::<Vec<_>>();
            preserves |= required.len() <= window_order.len()
                && required.iter().zip(&window_order).all(
                    |(
                        (required, descending, nulls),
                        (existing, existing_descending, existing_nulls),
                    )| {
                        exprs_match(required, existing)
                            && descending == existing_descending
                            && effective_nulls(*descending, *nulls)
                                == effective_nulls(*existing_descending, *existing_nulls)
                    },
                );
        });
    }
    Ok(preserves)
}

fn effective_nulls(descending: bool, nulls: Option<NullsOrder>) -> NullsOrder {
    nulls.unwrap_or(if descending {
        NullsOrder::First
    } else {
        NullsOrder::Last
    })
}
