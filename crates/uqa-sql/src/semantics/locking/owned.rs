//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! View row marks follow FROM subqueries while preserving independently owned plan fields.

use super::{RelationalPlan, ResolvedRowLock, SQLError, SourcePlan};
use crate::plan::{MutableQueryPlan, MutableSourcePlan, SourceChildSlot};
use uqa_core::memory::ProductionControl;

#[cfg(test)]
mod tests;

pub fn apply_propagated_view_lock_owned(
    query: MutableQueryPlan,
    target: &ResolvedRowLock,
    control: &ProductionControl<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    control.check()?;
    if !matches!(query.root, RelationalPlan::QueryBlock(_)) {
        return Ok(query);
    }
    query
        .append_row_lock(target.strength, target.wait, control)?
        .map_source(control, |source| propagate(source, target, control))
}

fn propagate(
    source: MutableSourcePlan,
    target: &ResolvedRowLock,
    control: &ProductionControl<'_>,
) -> Result<MutableSourcePlan, SQLError> {
    control.check()?;
    match &*source {
        SourcePlan::Join { .. } => source
            .map_child(SourceChildSlot::Left, control, |child| {
                propagate(child, target, control)
            })?
            .map_child(SourceChildSlot::Right, control, |child| {
                propagate(child, target, control)
            }),
        SourcePlan::Subquery { .. } => source.map_query(control, |query| {
            apply_propagated_view_lock_owned(query, target, control)
        }),
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. }
        | SourcePlan::RetainedRows(_) => Ok(source),
    }
}
