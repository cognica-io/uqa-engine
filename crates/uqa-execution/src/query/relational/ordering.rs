//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Projection placement around blocking ordering operators.

use super::limit::attach_order_limit;
use super::{build_set_projection, RelationalContext};
use crate::query::projection::projection_set_batch_size;
use crate::query::row_at_a_time::RowAtATime;
use crate::query::runtime::QueryRuntimeView;
use crate::query::{CteScope, OutputColumnMapping, PhysicalProjection};
use crate::{ProjectionTarget, SharedExpressionEvaluator};
use std::sync::Arc;
use uqa_core::Value;
use uqa_sql::semantics::sets::validation::projections_may_return_set;
use uqa_sql::{plan::QueryBlockPlan, SQLError, SQLParam, ScalarExpr};

pub struct FinalProjectionExecution<'context, 'scope, S: Clone + 'static> {
    pub context: RelationalContext<'context, S>,
    pub params: &'context [SQLParam],
    pub ctes: &'scope CteScope<S>,
    pub runtime: QueryRuntimeView<'context>,
    pub evaluator: SharedExpressionEvaluator<'context>,
}

pub use crate::query::ordering::*;

pub fn attach_final_projection_order<'a, S: Clone + 'static>(
    mut operator: Box<dyn crate::PhysicalOperator + 'a>,
    ordering: (&QueryBlockPlan, &[OutputColumnMapping]),
    projections: Vec<PhysicalProjection>,
    execution: FinalProjectionExecution<'a, '_, S>,
) -> Result<Box<dyn crate::PhysicalOperator + 'a>, SQLError> {
    let (statement, output) = ordering;
    let type_resolver = execution.context.expression_scope(execution.ctes.clone());
    let returns_set = projections_may_return_set(
        execution.context.catalog,
        type_resolver.as_ref(),
        &projections,
        operator.row_schema(),
        execution.params,
    )?;
    if execution.ctes.streams_command_progress() && !statement.order_by.is_empty() && !returns_set {
        return attach_deferred_order_projection(
            operator,
            statement,
            output,
            projections,
            execution,
        );
    }
    let FinalProjectionExecution {
        context,
        params,
        ctes,
        runtime,
        evaluator,
    } = execution;
    let batch_size = projection_set_batch_size(statement, ctes);
    operator = if returns_set {
        build_set_projection(
            operator,
            context,
            params,
            ctes,
            Arc::clone(&evaluator),
            crate::query::set_projection::SetProjectionOutput {
                projections,
                pass_through: false,
                batch_size,
            },
        )?
    } else {
        if batch_size == 1 {
            operator = Box::new(RowAtATime::new(operator));
        }
        Box::new(crate::Project::with_target_evaluator(
            operator,
            projections,
            Arc::clone(&evaluator),
        ))
    };
    attach_order_limit(
        operator, statement, output, context, params, ctes, runtime, evaluator, None,
    )
}

fn attach_deferred_order_projection<'a, S: Clone + 'static>(
    mut operator: Box<dyn crate::PhysicalOperator + 'a>,
    statement: &QueryBlockPlan,
    output: &[OutputColumnMapping],
    mut projections: Vec<PhysicalProjection>,
    execution: FinalProjectionExecution<'a, '_, S>,
) -> Result<Box<dyn crate::PhysicalOperator + 'a>, SQLError> {
    let FinalProjectionExecution {
        context,
        params,
        ctes,
        runtime,
        evaluator,
    } = execution;
    let mut sort_statement = statement.clone();
    let sort_relation = uqa_sql::ast::InternalRelationId::allocate();
    let mut sort_projections =
        Vec::<(Option<usize>, ScalarExpr, uqa_sql::ast::InternalColumnRef)>::new();
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
                    uqa_sql::semantics::aggregates::exprs_match(projected, &resolved)
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
                        || uqa_sql::semantics::aggregates::exprs_match(existing, &expression))
            })
            .map(|(_, _, column)| *column);
        let column = if let Some(column) = existing_column {
            column
        } else {
            let column = sort_relation.column(sort_projections.len());
            sort_projections.push((target, expression, column));
            column
        };
        sort_statement.order_by[order_index].expr = ScalarExpr::InternalColumn(column);
        if let Some(target) = target {
            projections[target].1 = ScalarExpr::InternalColumn(column);
        }
    }
    let sort_projections = sort_projections
        .into_iter()
        .map(|(_, expression, column)| (ProjectionTarget::Internal(column), expression))
        .collect();
    operator = Box::new(crate::Project::appending_target_evaluator(
        operator,
        sort_projections,
        Arc::clone(&evaluator),
    ));
    operator = attach_order_limit(
        operator,
        &sort_statement,
        &[],
        context,
        params,
        ctes,
        runtime,
        Arc::clone(&evaluator),
        None,
    )?;
    operator = Box::new(RowAtATime::new(operator));
    Ok(Box::new(crate::Project::with_target_evaluator(
        operator,
        projections,
        evaluator,
    )))
}
