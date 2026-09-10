//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Apply a bound projection to one physical row.
use super::RelationalContext;
use crate::query::{
    projection::{physical_exec_error, physical_projections},
    CteScope,
};
use uqa_sql::{plan::ProjectionPlan, SQLError, SQLParam};

pub fn build_projection_physical_row_with_ctes<S: Clone + 'static>(
    context: RelationalContext<'_, S>,
    input: &crate::OwnedPhysicalRow,
    projections: &[ProjectionPlan],
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<crate::OwnedPhysicalRow, SQLError> {
    use crate::physical::run_to_batches;
    use crate::scan::TableScan;
    use crate::{PhysicalOperator, Project};

    let scan: Box<dyn PhysicalOperator + '_> = Box::new(TableScan::from_physical_rows(
        input.schema.clone(),
        vec![input.row.clone()],
    ));
    let evaluator = context.evaluator(params, ctes);
    let mut project =
        Project::with_target_evaluator(scan, physical_projections(projections), evaluator);
    let mut rows = run_to_batches(&mut project)
        .map_err(physical_exec_error)?
        .into_iter()
        .flat_map(crate::Batch::into_owned_rows);
    let row = rows.next().ok_or_else(|| {
        SQLError::Internal("physical projection produced no row for a single-row input".into())
    })?;
    if rows.next().is_some() {
        return Err(SQLError::Internal(
            "physical projection produced multiple rows for a single-row input".into(),
        ));
    }
    Ok(row)
}
