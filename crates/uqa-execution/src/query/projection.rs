//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Projection expansion, output mapping, and physical execution utilities.

use crate::{PhysicalOperator, ProjectionTarget, ScalarExpr};
use uqa_sql::plan::{ProjectionPlan, QueryBlockPlan};
use uqa_sql::SQLError;

use crate::query::runtime::QueryRuntimeView;

use crate::query::{CteScope, PhysicalProjection};
use uqa_sql::semantics::projection_columns;

pub fn projection_set_batch_size<S: Clone>(
    statement: &QueryBlockPlan,
    ctes: &CteScope<S>,
) -> usize {
    if ctes.streams_command_progress()
        || statement.limit.is_some()
            && statement.order_by.is_empty()
            && !statement.distinct
            && statement.distinct_on.is_empty()
    {
        1
    } else {
        crate::DEFAULT_BATCH_SIZE
    }
}

pub use uqa_sql::semantics::expand_from_star_columns;

pub use uqa_sql::binding::catalog_sources::user_function_output_columns;

pub use crate::physical::physical_exec_error;

pub fn close_after_physical_failure(
    operator: &mut dyn PhysicalOperator,
    error: crate::ExecError,
    stage: &str,
) -> SQLError {
    match operator.close() {
        Ok(()) => physical_exec_error(error),
        Err(close_error) => SQLError::Internal(format!(
            "{error}; operator close after {stage} failure also failed: {close_error}"
        )),
    }
}

pub fn physical_work_mem_bytes(runtime: QueryRuntimeView<'_>) -> Result<usize, SQLError> {
    runtime.work_mem_bytes()
}

pub fn physical_projections(projections: &[ProjectionPlan]) -> Vec<PhysicalProjection> {
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

pub fn projection_target_expression(target: &ProjectionTarget) -> ScalarExpr {
    match target {
        ProjectionTarget::Column(column) => ScalarExpr::Column(column.clone()),
        ProjectionTarget::Internal(column) => ScalarExpr::InternalColumn(*column),
    }
}

pub use uqa_sql::semantics::expand_bound_projection_stars;

pub use uqa_sql::semantics::visible_projection_source_position;
