//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Aggregate, DISTINCT, and ordering-key preparation.

use uqa_execution::{FunctionTypeResolver, ProjectionTarget, RowSchema, ScalarExpr};
use uqa_planner::{ProjectionPlan, QueryBlockPlan};
use uqa_sql::ast::InternalColumnRef;
use uqa_sql::{SQLError, SQLParam};

use super::super::{
    expression_may_return_set, projection_columns, Engine, OutputColumnMapping, PhysicalProjection,
};
use super::ordering::{
    distinct_output_target_position, identity_order_columns, one_based_output_position,
    output_target_position, prior_distinct_key_index, resolve_order_expression,
};
use super::projection::projection_target_expression;

type PreparedOrderSetProjections = (Option<QueryBlockPlan>, Vec<(usize, InternalColumnRef)>);

pub(super) fn prepare_order_set_projections(
    engine: &Engine,
    type_resolver: &dyn FunctionTypeResolver,
    statement: &QueryBlockPlan,
    output_columns: &[OutputColumnMapping],
    projections: &mut Vec<PhysicalProjection>,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<PreparedOrderSetProjections, SQLError> {
    let mut prepared: Option<QueryBlockPlan> = None;
    let relation = uqa_sql::ast::InternalRelationId::allocate();
    let mut resjunk = Vec::new();
    for (index, order) in statement.order_by.iter().enumerate() {
        if let Some(target) = output_target_position(statement, &order.expr, output_columns)? {
            if !target.direct {
                prepared.get_or_insert_with(|| statement.clone()).order_by[index].expr =
                    one_based_output_position(target.position)?;
            }
            continue;
        }
        let expression = resolve_order_expression(&order.expr, output_columns)?;
        if statement.with_ties {
            let column = relation.column(resjunk.len());
            projections.push((ProjectionTarget::Internal(column), expression));
            resjunk.push((index, column));
            prepared.get_or_insert_with(|| statement.clone()).order_by[index].expr =
                ScalarExpr::InternalColumn(column);
            continue;
        }
        if let Some((target, _)) = projections
            .iter()
            .find(|(_, projected)| crate::sql::aggregates::exprs_match(projected, &expression))
        {
            prepared.get_or_insert_with(|| statement.clone()).order_by[index].expr =
                projection_target_expression(target);
        } else if expression_may_return_set(engine, type_resolver, &expression, schema, params)? {
            let column = relation.column(resjunk.len());
            projections.push((ProjectionTarget::Internal(column), expression));
            resjunk.push((index, column));
            prepared.get_or_insert_with(|| statement.clone()).order_by[index].expr =
                ScalarExpr::InternalColumn(column);
        }
    }
    Ok((prepared, resjunk))
}

pub(super) fn append_distinct_set_projections(
    statement: &QueryBlockPlan,
    output_columns: &[OutputColumnMapping],
    projections: &mut Vec<PhysicalProjection>,
) -> Result<Vec<(usize, InternalColumnRef)>, SQLError> {
    let mut columns = Vec::new();
    let relation = uqa_sql::ast::InternalRelationId::allocate();
    for (index, expression) in statement.distinct_on.iter().enumerate() {
        if prior_distinct_key_index(statement, index, expression, output_columns)?.is_some() {
            continue;
        }
        if distinct_output_target_position(statement, expression, output_columns)?.is_some() {
            continue;
        }
        let expression = resolve_order_expression(expression, output_columns)?;
        let column = relation.column(columns.len());
        projections.push((ProjectionTarget::Internal(column), expression));
        columns.push((index, column));
    }
    Ok(columns)
}

pub(super) struct AggregateKeyStatement {
    pub(super) statement: QueryBlockPlan,
    pub(super) targets: Vec<(usize, InternalColumnRef)>,
    pub(super) distinct_on: Vec<(usize, InternalColumnRef)>,
    pub(super) order_by: Vec<(usize, InternalColumnRef)>,
}

pub(super) fn prepare_aggregate_key_statement(
    statement: &QueryBlockPlan,
) -> Result<Option<AggregateKeyStatement>, SQLError> {
    let output = identity_order_columns(&projection_columns(&statement.projections));
    let mut prepared: Option<QueryBlockPlan> = None;
    let relation = uqa_sql::ast::InternalRelationId::allocate();
    let mut targets = Vec::new();
    let mut distinct_on = Vec::new();
    let mut order_by = Vec::new();
    for (index, expression) in statement.distinct_on.iter().enumerate() {
        if prior_distinct_key_index(statement, index, expression, &output)?.is_some() {
            continue;
        }
        if distinct_output_target_position(statement, expression, &output)?.is_some() {
            continue;
        }
        let expression = resolve_order_expression(expression, &output)?;
        let prepared = prepared.get_or_insert_with(|| statement.clone());
        let position = prepared.projections.len();
        let column = relation.column(targets.len());
        prepared.projections.push(ProjectionPlan {
            expr: expression,
            alias: None,
        });
        targets.push((position, column));
        distinct_on.push((index, column));
    }
    for (index, order) in statement.order_by.iter().enumerate() {
        if let Some(target) = output_target_position(statement, &order.expr, &output)? {
            if !target.direct {
                prepared.get_or_insert_with(|| statement.clone()).order_by[index].expr =
                    one_based_output_position(target.position)?;
            }
            continue;
        }
        let expression = resolve_order_expression(&order.expr, &output)?;
        let current = prepared.as_ref().unwrap_or(statement);
        let existing = current
            .projections
            .iter()
            .enumerate()
            .find(|(_, projection)| {
                crate::sql::aggregates::exprs_match(&projection.expr, &expression)
            })
            .map(|(position, _)| position);
        let prepared = prepared.get_or_insert_with(|| statement.clone());
        let key = if let Some(position) = existing {
            if let Some((_, column)) = targets
                .iter()
                .find(|(target_position, _)| *target_position == position)
            {
                order_by.push((index, *column));
                ScalarExpr::InternalColumn(*column)
            } else {
                ScalarExpr::Position(position)
            }
        } else {
            let position = prepared.projections.len();
            let column = relation.column(targets.len());
            prepared.projections.push(ProjectionPlan {
                expr: expression,
                alias: None,
            });
            targets.push((position, column));
            order_by.push((index, column));
            ScalarExpr::InternalColumn(column)
        };
        prepared.order_by[index].expr = key;
    }
    Ok(prepared.map(|statement| AggregateKeyStatement {
        statement,
        targets,
        distinct_on,
        order_by,
    }))
}
