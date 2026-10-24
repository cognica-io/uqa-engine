//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Projection placement around blocking ordering operators.

use super::limit::attach_order_limit;
use super::{build_set_projection, RelationalContext};
use crate::physical::construction::OperatorBuild;
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
mod streaming;
pub(super) use streaming::attach_streaming_order_projection;

pub fn attach_final_projection_order<'a, S: Clone + 'static>(
    operator: Box<dyn crate::PhysicalOperator + 'a>,
    ordering: (&QueryBlockPlan, &[OutputColumnMapping]),
    projections: Vec<PhysicalProjection>,
    execution: FinalProjectionExecution<'a, '_, S>,
) -> Result<Box<dyn crate::PhysicalOperator + 'a>, SQLError> {
    OperatorBuild::build(operator, |operator| {
        let (statement, output) = ordering;
        let type_resolver = execution.context.expression_scope(execution.ctes.clone());
        let returns_set = projections_may_return_set(
            execution.context.catalog,
            type_resolver.as_ref(),
            &projections,
            operator.row_schema(),
            execution.params,
        )?;
        if execution.ctes.streams_command_progress()
            && !statement.order_by.is_empty()
            && !returns_set
        {
            return operator.attach(|input| {
                attach_deferred_order_projection(input, statement, output, projections, execution)
            });
        }
        let FinalProjectionExecution {
            context,
            params,
            ctes,
            runtime,
            evaluator,
        } = execution;
        let batch_size = projection_set_batch_size(statement, ctes);
        if returns_set {
            operator.attach(|input| {
                build_set_projection(
                    input,
                    context,
                    params,
                    ctes,
                    Arc::clone(&evaluator),
                    crate::query::set_projection::SetProjectionOutput {
                        projections,
                        pass_through: false,
                        batch_size,
                    },
                )
            })?;
        } else {
            if batch_size == 1 {
                operator.wrap(|input| Box::new(RowAtATime::new(input)));
            }
            let work_mem = runtime.work_mem_bytes()?;
            operator.wrap(|input| {
                Box::new(
                    crate::Project::with_target_evaluator(
                        input,
                        projections,
                        Arc::clone(&evaluator),
                    )
                    .with_memory_budget(work_mem),
                )
            });
        }
        operator.attach(|input| {
            attach_order_limit(
                input, statement, output, context, params, ctes, runtime, evaluator, None,
            )
        })
    })
}

fn attach_deferred_order_projection<'a, S: Clone + 'static>(
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
            evaluator,
        } = execution;
        let (sort_statement, sort_projections, projections) =
            prepare_deferred_order_projection(statement, output, projections)?;
        let work_mem = runtime.work_mem_bytes()?;
        operator.wrap(|input| {
            Box::new(
                crate::Project::appending_target_evaluator(
                    input,
                    sort_projections,
                    Arc::clone(&evaluator),
                )
                .with_memory_budget(work_mem),
            )
        });
        operator.attach(|input| {
            attach_order_limit(
                input,
                &sort_statement,
                &[],
                context,
                params,
                ctes,
                runtime,
                Arc::clone(&evaluator),
                None,
            )
        })?;
        operator.wrap(|input| Box::new(RowAtATime::new(input)));
        let work_mem = runtime.work_mem_bytes()?;
        operator.wrap(|input| {
            Box::new(
                crate::Project::with_target_evaluator(input, projections, evaluator)
                    .with_memory_budget(work_mem),
            )
        });
        Ok(())
    })
}

type PreparedDeferredOrderProjection = (
    QueryBlockPlan,
    Vec<PhysicalProjection>,
    Vec<PhysicalProjection>,
);

pub(super) fn prepare_deferred_order_projection(
    statement: &QueryBlockPlan,
    output: &[OutputColumnMapping],
    mut projections: Vec<PhysicalProjection>,
) -> Result<PreparedDeferredOrderProjection, SQLError> {
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
                projections.iter().position(|(target, projected)| {
                    matches!(
                        (target, &resolved),
                        (ProjectionTarget::Internal(column), ScalarExpr::InternalColumn(required))
                            if column == required
                    ) || uqa_sql::semantics::aggregates::exprs_match(projected, &resolved)
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
    Ok((sort_statement, sort_projections, projections))
}
