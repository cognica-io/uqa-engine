//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Place streaming target sets before or after sorting without splitting their evaluation level.

use super::{append_row_at_time_projection, order_projection_targets, FinalProjectionExecution};
use crate::physical::construction::OperatorBuild;
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
    operator: Box<dyn crate::PhysicalOperator + 'a>,
    statement: &QueryBlockPlan,
    output: &[OutputColumnMapping],
    projections: Vec<PhysicalProjection>,
    execution: FinalProjectionExecution<'a, '_, S>,
) -> Result<Box<dyn crate::PhysicalOperator + 'a>, SQLError> {
    OperatorBuild::build(operator, |operator| {
        let FinalProjectionExecution {
            context,
            params,
            ctes,
            runtime,
            ref evaluator,
        } = execution;
        let (mut sort_statement, required) =
            order_projection_targets(statement, output, &projections)?;
        let setness = projection_setness(&projections, operator.row_schema(), &execution)?;
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
            operator.attach(|input| {
                build_set_projection(
                    input,
                    context,
                    params,
                    ctes,
                    Arc::clone(evaluator),
                    SetProjectionOutput {
                        projections: before_sort,
                        pass_through: true,
                        batch_size: projection_set_batch_size(statement, ctes),
                    },
                )
            })?;
        } else if !before_sort.is_empty() {
            let work_mem = runtime.work_mem_bytes()?;
            operator.wrap(|input| {
                Box::new(
                    crate::Project::appending_target_evaluator(
                        input,
                        before_sort,
                        Arc::clone(evaluator),
                    )
                    .with_memory_budget(work_mem),
                )
            });
        }
        // A postponed target set expands the sorted input before OFFSET/LIMIT counts output rows.
        if sets_after_sort {
            sort_statement.limit = None;
            sort_statement.offset = None;
            sort_statement.with_ties = false;
        }
        operator.attach(|input| {
            attach_order_limit(
                input,
                &sort_statement,
                output,
                context,
                params,
                ctes,
                runtime,
                Arc::clone(evaluator),
                None,
            )
        })?;
        if sets_after_sort {
            operator.attach(|input| {
                build_set_projection(
                    input,
                    context,
                    params,
                    ctes,
                    Arc::clone(evaluator),
                    SetProjectionOutput {
                        projections: after_sort,
                        pass_through: true,
                        batch_size: projection_set_batch_size(statement, ctes),
                    },
                )
            })?;
            operator.attach(|input| attach_presorted_limit(input, statement, output, execution))?;
        } else {
            operator.wrap(|input| {
                append_row_at_time_projection(input, after_sort, execution.evaluator)
            });
        }
        Ok(())
    })
}

fn projection_setness<S: Clone + 'static>(
    projections: &[PhysicalProjection],
    schema: &crate::RowSchema,
    execution: &FinalProjectionExecution<'_, '_, S>,
) -> Result<Vec<bool>, SQLError> {
    let type_resolver = execution.context.expression_scope(execution.ctes.clone());
    projections
        .iter()
        .map(|(_, expression)| {
            expression_may_return_set(
                execution.context.catalog,
                type_resolver.as_ref(),
                expression,
                schema,
                execution.params,
            )
        })
        .collect()
}
