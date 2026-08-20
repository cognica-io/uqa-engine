//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Projection placement around blocking ordering operators.

use std::collections::HashSet;
use std::sync::Arc;

use super::{
    attach_order_limit, build_set_projection, identity_order_columns, projection_columns,
    projection_set_batch_size, projections_may_return_set, resolve_order_expression, CteScope,
    Engine, OutputColumnMapping, PhysicalProjection, QueryBlockPlan, RowAtATime, SQLError,
    SQLParam, ScalarExpr, SharedExpressionEvaluator, Value,
};

const DEFERRED_ORDER_COLUMN_PREFIX: &str = "\0uqa.deferred_order_value.";

#[derive(Clone, Copy)]
pub(super) struct OutputTarget {
    pub(super) position: usize,
    pub(super) direct: bool,
}

pub(super) fn output_target_position(
    statement: &QueryBlockPlan,
    expression: &ScalarExpr,
    output: &[OutputColumnMapping],
) -> Result<Option<OutputTarget>, SQLError> {
    output_target_position_for(statement, expression, output, "ORDER BY")
}

pub(super) fn distinct_output_target_position(
    statement: &QueryBlockPlan,
    expression: &ScalarExpr,
    output: &[OutputColumnMapping],
) -> Result<Option<OutputTarget>, SQLError> {
    output_target_position_for(statement, expression, output, "DISTINCT ON")
}

fn output_target_position_for(
    statement: &QueryBlockPlan,
    expression: &ScalarExpr,
    output: &[OutputColumnMapping],
    clause: &str,
) -> Result<Option<OutputTarget>, SQLError> {
    match expression {
        ScalarExpr::Literal(Value::Int(position)) => {
            let position = usize::try_from(*position)
                .ok()
                .and_then(|position| position.checked_sub(1))
                .filter(|position| *position < output.len())
                .ok_or_else(|| output_position_error(clause, *position))?;
            return Ok(Some(OutputTarget {
                position,
                direct: true,
            }));
        }
        ScalarExpr::Column(name) => {
            let mut matches = output
                .iter()
                .enumerate()
                .filter(|(_, (label, _))| label == name);
            if let Some((position, _)) = matches.next() {
                if matches.next().is_some() {
                    return Err(SQLError::AmbiguousColumn(name.clone()));
                }
                return Ok(Some(OutputTarget {
                    position,
                    direct: true,
                }));
            }
        }
        _ => {}
    }
    if statement.projections.len() != output.len() {
        return Ok(None);
    }
    Ok(statement
        .projections
        .iter()
        .position(|projection| crate::sql::aggregates::exprs_match(&projection.expr, expression))
        .map(|position| OutputTarget {
            position,
            direct: false,
        }))
}

pub(super) fn output_position_error(clause: &str, position: i64) -> SQLError {
    SQLError::Routine {
        sqlstate: "42P10".into(),
        message: format!("{clause} position {position} is not in the select list"),
    }
}

pub(super) fn one_based_output_position(position: usize) -> Result<ScalarExpr, SQLError> {
    let position = position
        .checked_add(1)
        .and_then(|position| i64::try_from(position).ok())
        .ok_or_else(|| SQLError::Internal("SELECT output position exceeds i64".into()))?;
    Ok(ScalarExpr::Literal(Value::Int(position)))
}

fn distinct_key_expressions_match(
    statement: &QueryBlockPlan,
    left: &ScalarExpr,
    right: &ScalarExpr,
    output: &[OutputColumnMapping],
    right_is_order_by: bool,
) -> Result<bool, SQLError> {
    let left_target = distinct_output_target_position(statement, left, output)?;
    let right_target = if right_is_order_by {
        output_target_position(statement, right, output)?
    } else {
        distinct_output_target_position(statement, right, output)?
    };
    match (left_target, right_target) {
        (Some(left), Some(right)) => Ok(left.position == right.position),
        (None, None) => Ok(crate::sql::aggregates::exprs_match(
            &resolve_order_expression(left, output)?,
            &resolve_order_expression(right, output)?,
        )),
        _ => Ok(false),
    }
}

pub(super) fn prior_distinct_key_index(
    statement: &QueryBlockPlan,
    index: usize,
    expression: &ScalarExpr,
    output: &[OutputColumnMapping],
) -> Result<Option<usize>, SQLError> {
    for (prior, candidate) in statement.distinct_on[..index].iter().enumerate() {
        if distinct_key_expressions_match(statement, candidate, expression, output, false)? {
            return Ok(Some(prior));
        }
    }
    Ok(None)
}

pub(super) fn validate_distinct_ordering(statement: &QueryBlockPlan) -> Result<(), SQLError> {
    if !statement.distinct || statement.order_by.is_empty() {
        return Ok(());
    }
    let output = identity_order_columns(&projection_columns(&statement.projections));
    if statement.distinct_on.is_empty() {
        if statement
            .order_by
            .iter()
            .try_fold(false, |invalid, order| {
                Ok::<_, SQLError>(
                    invalid || output_target_position(statement, &order.expr, &output)?.is_none(),
                )
            })?
        {
            return Err(SQLError::Routine {
                sqlstate: "42P10".into(),
                message: "for SELECT DISTINCT, ORDER BY expressions must appear in select list"
                    .into(),
            });
        }
        return Ok(());
    }
    let mut matched = vec![false; statement.distinct_on.len()];
    let mut encountered_non_distinct = false;
    for order in &statement.order_by {
        let mut order_is_distinct = false;
        for (index, expression) in statement.distinct_on.iter().enumerate() {
            if distinct_key_expressions_match(statement, expression, &order.expr, &output, true)? {
                order_is_distinct = true;
                matched[index] = true;
            }
        }
        if order_is_distinct {
            if encountered_non_distinct {
                return Err(distinct_on_ordering_error());
            }
        } else {
            encountered_non_distinct = true;
        }
    }
    if encountered_non_distinct && matched.iter().any(|matched| !matched) {
        return Err(distinct_on_ordering_error());
    }
    Ok(())
}

fn distinct_on_ordering_error() -> SQLError {
    SQLError::Routine {
        sqlstate: "42P10".into(),
        message: "SELECT DISTINCT ON expressions must match initial ORDER BY expressions".into(),
    }
}

pub(super) fn split_locking_order_projections(
    statement: &QueryBlockPlan,
    output: &[OutputColumnMapping],
    physical: Vec<PhysicalProjection>,
) -> Result<
    (
        QueryBlockPlan,
        Vec<PhysicalProjection>,
        Vec<PhysicalProjection>,
    ),
    SQLError,
> {
    let mut sort_statement = statement.clone();
    let mut required = HashSet::new();
    for (index, order) in statement.order_by.iter().enumerate() {
        let expression = resolve_order_expression(&order.expr, output)?;
        if let ScalarExpr::Column(column) = &expression {
            if physical.iter().any(|(name, _)| name == column) {
                required.insert(column.clone());
                continue;
            }
        }
        if let Some((column, _)) = physical
            .iter()
            .find(|(_, projected)| crate::sql::aggregates::exprs_match(projected, &expression))
        {
            required.insert(column.clone());
            sort_statement.order_by[index].expr = ScalarExpr::Column(column.clone());
        }
    }
    let (before_sort, after_sort) = physical
        .into_iter()
        .partition(|(column, _)| required.contains(column));
    Ok((sort_statement, before_sort, after_sort))
}

pub(super) fn append_row_at_time_projection<'a>(
    operator: Box<dyn uqa_execution::PhysicalOperator + 'a>,
    projections: Vec<PhysicalProjection>,
    evaluator: SharedExpressionEvaluator<'a>,
) -> Box<dyn uqa_execution::PhysicalOperator + 'a> {
    if projections.is_empty() {
        return operator;
    }
    Box::new(uqa_execution::Project::appending_with_evaluator(
        Box::new(RowAtATime::new(operator)),
        projections,
        evaluator,
    ))
}

pub(super) fn attach_final_projection_order<'a>(
    mut operator: Box<dyn uqa_execution::PhysicalOperator + 'a>,
    ordering: (&QueryBlockPlan, &[OutputColumnMapping]),
    projections: Vec<PhysicalProjection>,
    engine: &'a Engine,
    params: &'a [SQLParam],
    ctes: &CteScope,
    evaluator: SharedExpressionEvaluator<'a>,
) -> Result<Box<dyn uqa_execution::PhysicalOperator + 'a>, SQLError> {
    let (statement, output) = ordering;
    let returns_set =
        projections_may_return_set(engine, &projections, operator.row_schema(), params)?;
    if ctes.streams_command_progress() && !statement.order_by.is_empty() && !returns_set {
        return attach_deferred_order_projection(
            operator,
            statement,
            output,
            projections,
            engine,
            params,
            ctes,
            evaluator,
        );
    }
    let batch_size = projection_set_batch_size(statement, ctes);
    operator = if returns_set {
        build_set_projection(
            operator,
            engine,
            params,
            ctes,
            Arc::clone(&evaluator),
            projections,
            false,
            batch_size,
        )?
    } else {
        if batch_size == 1 {
            operator = Box::new(RowAtATime::new(operator));
        }
        Box::new(uqa_execution::Project::with_evaluator(
            operator,
            projections,
            Arc::clone(&evaluator),
        ))
    };
    attach_order_limit(
        operator, statement, output, engine, params, ctes, evaluator, None,
    )
}

#[allow(clippy::too_many_arguments)]
fn attach_deferred_order_projection<'a>(
    mut operator: Box<dyn uqa_execution::PhysicalOperator + 'a>,
    statement: &QueryBlockPlan,
    output: &[OutputColumnMapping],
    mut projections: Vec<PhysicalProjection>,
    engine: &'a Engine,
    params: &'a [SQLParam],
    ctes: &CteScope,
    evaluator: SharedExpressionEvaluator<'a>,
) -> Result<Box<dyn uqa_execution::PhysicalOperator + 'a>, SQLError> {
    let mut sort_statement = statement.clone();
    let mut sort_projections = Vec::<(Option<usize>, ScalarExpr, String)>::new();
    for (order_index, order) in statement.order_by.iter().enumerate() {
        let resolved = resolve_order_expression(&order.expr, output)?;
        let direct_target = match &order.expr {
            ScalarExpr::Literal(Value::Int(position)) => usize::try_from(*position)
                .ok()
                .and_then(|position| position.checked_sub(1)),
            ScalarExpr::Column(name) => output.iter().position(|(label, _)| label == name),
            _ => None,
        };
        let target = direct_target
            .or_else(|| {
                projections.iter().position(|(_, projected)| {
                    crate::sql::aggregates::exprs_match(projected, &resolved)
                })
            })
            .filter(|position| *position < projections.len());
        let expression = target
            .map(|position| projections[position].1.clone())
            .unwrap_or(resolved);
        let existing_column = sort_projections
            .iter()
            .find(|(existing_target, existing, _)| {
                target == *existing_target
                    && (target.is_some()
                        || crate::sql::aggregates::exprs_match(existing, &expression))
            })
            .map(|(_, _, column)| column.clone());
        let column = if let Some(column) = existing_column {
            column
        } else {
            let column = format!("{DEFERRED_ORDER_COLUMN_PREFIX}{order_index}");
            sort_projections.push((target, expression, column.clone()));
            column
        };
        sort_statement.order_by[order_index].expr = ScalarExpr::Column(column.clone());
        if let Some(target) = target {
            projections[target].1 = ScalarExpr::Column(column);
        }
    }
    let sort_projections = sort_projections
        .into_iter()
        .map(|(_, expression, column)| (column, expression))
        .collect();
    operator = Box::new(uqa_execution::Project::appending_with_evaluator(
        operator,
        sort_projections,
        Arc::clone(&evaluator),
    ));
    operator = attach_order_limit(
        operator,
        &sort_statement,
        &[],
        engine,
        params,
        ctes,
        Arc::clone(&evaluator),
        None,
    )?;
    operator = Box::new(RowAtATime::new(operator));
    Ok(Box::new(uqa_execution::Project::with_evaluator(
        operator,
        projections,
        evaluator,
    )))
}
