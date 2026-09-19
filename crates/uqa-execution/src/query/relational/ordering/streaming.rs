//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Place streaming target sets before or after sorting without splitting their evaluation level.

use super::{append_row_at_time_projection, order_projection_targets, FinalProjectionExecution};
use crate::query::{
    projection::projection_set_batch_size,
    relational::{
        build_set_projection,
        limit::{attach_order_limit, attach_presorted_limit},
    },
    set_projection::SetProjectionOutput,
    OutputColumnMapping, PhysicalProjection,
};
use std::sync::Arc;
use uqa_sql::{
    plan::QueryBlockPlan, semantics::sets::validation::expression_may_return_set, SQLError,
};

pub(crate) fn attach_streaming_order_projection<'a, S: Clone + 'static>(
    mut operator: Box<dyn crate::PhysicalOperator + 'a>,
    statement: &QueryBlockPlan,
    output: &[OutputColumnMapping],
    projections: Vec<PhysicalProjection>,
    execution: FinalProjectionExecution<'a, '_, S>,
) -> Result<Box<dyn crate::PhysicalOperator + 'a>, SQLError> {
    let FinalProjectionExecution {
        context,
        params,
        ctes,
        runtime,
        ref evaluator,
    } = execution;
    let type_resolver = context.expression_scope(ctes.clone());
    let (mut sort_statement, required) = order_projection_targets(statement, output, &projections)?;
    let setness = projections
        .iter()
        .map(|(_, expression)| {
            expression_may_return_set(
                context.catalog,
                type_resolver.as_ref(),
                expression,
                operator.row_schema(),
                params,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let sets_before_sort = projections
        .iter()
        .zip(&setness)
        .any(|((target, _), returns_set)| *returns_set && required.contains(target));
    let sets_after_sort = !sets_before_sort && setness.contains(&true);
    let mut before_sort = Vec::new();
    let mut after_sort = Vec::new();
    for (projection, returns_set) in projections.into_iter().zip(setness) {
        if required.contains(&projection.0) || (sets_before_sort && returns_set) {
            before_sort.push(projection);
        } else {
            after_sort.push(projection);
        }
    }
    if sets_before_sort {
        operator = build_set_projection(
            operator,
            context,
            params,
            ctes,
            Arc::clone(evaluator),
            SetProjectionOutput {
                projections: before_sort,
                pass_through: true,
                batch_size: projection_set_batch_size(statement, ctes),
            },
        )?;
    } else if !before_sort.is_empty() {
        operator = Box::new(crate::Project::appending_target_evaluator(
            operator,
            before_sort,
            Arc::clone(evaluator),
        ));
    }
    // A postponed target set expands the sorted input before OFFSET/LIMIT counts output rows.
    if sets_after_sort {
        sort_statement.limit = None;
        sort_statement.offset = None;
        sort_statement.with_ties = false;
    }
    operator = attach_order_limit(
        operator,
        &sort_statement,
        output,
        context,
        params,
        ctes,
        runtime,
        Arc::clone(evaluator),
        None,
    )?;
    if sets_after_sort {
        operator = build_set_projection(
            operator,
            context,
            params,
            ctes,
            Arc::clone(evaluator),
            SetProjectionOutput {
                projections: after_sort,
                pass_through: true,
                batch_size: projection_set_batch_size(statement, ctes),
            },
        )?;
        attach_presorted_limit(operator, statement, output, execution)
    } else {
        Ok(append_row_at_time_projection(
            operator,
            after_sort,
            execution.evaluator,
        ))
    }
}
