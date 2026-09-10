//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Projection expansion, output mapping, and physical execution utilities.

use uqa_execution::{PhysicalOperator, ProjectionTarget, ScalarExpr};
use uqa_planner::{ProjectionPlan, QueryBlockPlan};
use uqa_sql::SQLError;

use crate::engine_capabilities::QueryRuntimeView;

use super::super::{projection_columns, CteScope, PhysicalProjection};

pub(super) fn projection_set_batch_size(statement: &QueryBlockPlan, ctes: &CteScope) -> usize {
    if ctes.streams_command_progress()
        || statement.limit.is_some()
            && statement.order_by.is_empty()
            && !statement.distinct
            && statement.distinct_on.is_empty()
    {
        1
    } else {
        uqa_execution::DEFAULT_BATCH_SIZE
    }
}

pub(in crate::sql) use uqa_sql::semantics::expand_from_star_columns;

pub(in crate::sql) use uqa_sql::binding::catalog_sources::user_function_output_columns;

pub(in crate::sql) fn physical_exec_error(error: uqa_execution::ExecError) -> SQLError {
    match error {
        uqa_execution::ExecError::SQL(error) => error,
        uqa_execution::ExecError::Other(message) => SQLError::Internal(message),
    }
}

pub(in crate::sql) fn close_after_physical_failure(
    operator: &mut dyn PhysicalOperator,
    error: uqa_execution::ExecError,
    stage: &str,
) -> SQLError {
    match operator.close() {
        Ok(()) => physical_exec_error(error),
        Err(close_error) => SQLError::Internal(format!(
            "{error}; operator close after {stage} failure also failed: {close_error}"
        )),
    }
}

pub(in crate::sql) fn physical_work_mem_bytes(
    runtime: QueryRuntimeView<'_>,
) -> Result<usize, SQLError> {
    runtime.work_mem_bytes()
}

pub(in crate::sql) fn physical_projections(
    projections: &[ProjectionPlan],
) -> Vec<PhysicalProjection> {
    let labels = projection_columns(projections);
    projections
        .iter()
        .enumerate()
        .map(|(index, projection)| {
            (
                ProjectionTarget::Column(labels[index].clone()),
                match &projection.expr {
                    ScalarExpr::Literal(uqa_core::Value::Null) => ScalarExpr::Cast {
                        expr: Box::new(projection.expr.clone()),
                        ty: "text".into(),
                    },
                    expression => expression.clone(),
                },
            )
        })
        .collect()
}

pub(super) fn projection_target_expression(target: &ProjectionTarget) -> ScalarExpr {
    match target {
        ProjectionTarget::Column(column) => ScalarExpr::Column(column.clone()),
        ProjectionTarget::Internal(column) => ScalarExpr::InternalColumn(*column),
    }
}

pub(in crate::sql) use uqa_sql::semantics::expand_bound_projection_stars;

pub(in crate::sql) use uqa_sql::semantics::visible_projection_source_position;
