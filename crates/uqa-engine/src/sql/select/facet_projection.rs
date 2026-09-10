//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Supply scoped expression services to projection and row-count execution.

use super::{CteScope, Engine, ProjectionPlan, SQLError, SQLParam};

pub(in crate::sql) fn build_projection_physical_row_with_ctes(
    engine: &Engine,
    input: &uqa_execution::OwnedPhysicalRow,
    projections: &[ProjectionPlan],
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<uqa_execution::OwnedPhysicalRow, SQLError> {
    uqa_execution::query::relational::project_row::build_projection_physical_row_with_ctes(
        engine.relational_context(),
        input,
        projections,
        params,
        ctes,
    )
}
