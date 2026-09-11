//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind scalar-subquery result types from a physical plan arena.

use crate::query::CteScope;
use crate::RowSchema;
use uqa_sql::{routines::RoutineResolution, ColumnType, SQLError, SQLParam, SubqueryId};

pub fn resolve_scalar_subquery_type<S: Clone>(
    routines: &dyn RoutineResolution,
    subquery: SubqueryId,
    outer_schema: &RowSchema,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<Option<ColumnType>, SQLError> {
    let plan = ctes.scalar_subqueries.get(subquery).ok_or_else(|| {
        SQLError::Internal(format!(
            "physical scalar subquery slot {subquery} is out of bounds"
        ))
    })?;
    let output = super::bind_query_plan_schema(routines, plan, params, ctes, Some(outer_schema))?;
    Ok(output.column_type(0).cloned())
}

#[cfg(test)]
mod tests;
