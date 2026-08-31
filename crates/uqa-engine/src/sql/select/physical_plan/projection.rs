//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Projection expansion, output mapping, and physical execution utilities.

use uqa_execution::{PhysicalOperator, ProjectionTarget, RowSchema, ScalarExpr};
use uqa_planner::{ProjectionPlan, QueryBlockPlan};
use uqa_sql::SQLError;

use crate::engine_capabilities::{CatalogReadView, QueryRuntimeView, RelationNameResolution};

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

pub(in crate::sql) fn expand_from_star_columns(
    columns: Vec<String>,
    projections: &[ProjectionPlan],
    source_schema: &RowSchema,
) -> Result<Vec<String>, SQLError> {
    let mut output = Vec::new();
    for (position, projection) in projections.iter().enumerate() {
        match &projection.expr {
            ScalarExpr::Star => {
                output.extend(
                    source_schema
                        .columns()
                        .iter()
                        .enumerate()
                        .filter(|(position, _)| {
                            visible_projection_source_position(source_schema, *position)
                        })
                        .map(|(source_position, column)| {
                            source_schema
                                .public_name(source_position)
                                .unwrap_or(column)
                                .to_string()
                        }),
                );
            }
            ScalarExpr::QualifiedStar(qualifier) => {
                let qualified_columns = source_schema
                    .qualified_star_position_layout(qualifier)
                    .into_iter()
                    .filter(|(_, logical, _, _)| {
                        logical.is_none_or(|position| {
                            visible_projection_source_position(source_schema, position)
                        })
                    })
                    .map(|(column, _, _, _)| column)
                    .collect::<Vec<_>>();
                if qualified_columns.is_empty() {
                    return Err(SQLError::UnknownTable(qualifier.clone()));
                }
                output.extend(qualified_columns);
            }
            _ => output.push(columns[position].clone()),
        }
    }
    Ok(output)
}

/// Output column names of a user-defined routine used as a FROM source: OUT / INOUT / `RETURNS TABLE` parameter names. `None` when the name is not a user routine or its result is a single unnamed column, which keeps the function-name default.
pub(in crate::sql) fn user_function_output_columns(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    name: &str,
) -> Result<Option<Vec<String>>, SQLError> {
    let Some(overloads) = catalog.sql_functions(resolution, name)? else {
        return Ok(None);
    };
    for function in &overloads {
        if let Some(columns) = crate::sql::from_rows::user_function_output_columns_for(function) {
            return Ok(Some(columns));
        }
    }
    Ok(None)
}

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
                projection.expr.clone(),
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

fn bound_projection_expression(schema: &RowSchema, position: usize) -> ScalarExpr {
    let Some(identity) = schema.identity(position) else {
        return ScalarExpr::Position(position);
    };
    if let Some(qualifier) = identity.qualifier() {
        if schema.qualified_position(qualifier, identity.column()) == Some(position) {
            return ScalarExpr::qualified_column(qualifier, identity.column());
        }
    } else if schema.unqualified_position(identity.column()) == Some(position) {
        return ScalarExpr::Column(identity.column().to_string());
    }
    ScalarExpr::Position(position)
}

pub(in crate::sql) fn expand_bound_projection_stars(
    projections: &[ProjectionPlan],
    schema: &RowSchema,
) -> Result<Vec<ProjectionPlan>, SQLError> {
    let mut expanded = Vec::new();
    for projection in projections {
        match &projection.expr {
            ScalarExpr::Star => {
                for (position, column) in schema.columns().iter().enumerate() {
                    if !visible_projection_source_position(schema, position) {
                        continue;
                    }
                    expanded.push(ProjectionPlan {
                        expr: bound_projection_expression(schema, position),
                        alias: Some(schema.public_name(position).unwrap_or(column).to_string()),
                    });
                }
            }
            ScalarExpr::QualifiedStar(qualifier) => {
                let layout = schema.qualified_star_position_layout(qualifier);
                if layout.is_empty() {
                    return Err(SQLError::UnknownTable(qualifier.clone()));
                }
                for (column, logical, _, _) in layout {
                    if logical.is_some_and(|position| {
                        !visible_projection_source_position(schema, position)
                    }) {
                        continue;
                    }
                    expanded.push(ProjectionPlan {
                        expr: logical.map_or_else(
                            || ScalarExpr::qualified_column(qualifier, &column),
                            |position| bound_projection_expression(schema, position),
                        ),
                        alias: Some(column),
                    });
                }
            }
            _ => expanded.push(projection.clone()),
        }
    }
    Ok(expanded)
}

pub(in crate::sql) fn visible_projection_source_position(
    schema: &RowSchema,
    position: usize,
) -> bool {
    schema.wildcard_position_visible(position)
}
