//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Classify immutable document projection metadata before row production.

use crate::ProjectedValueSlot;
use uqa_sql::ast::ColumnDef;

pub(super) struct ProjectionProperties {
    pub(super) requires_materialization: bool,
    pub(super) identity: bool,
}

impl ProjectionProperties {
    pub(super) fn new(
        definitions: &[ColumnDef],
        fields: &[String],
        slots: &[ProjectedValueSlot],
    ) -> Self {
        Self {
            requires_materialization: requires_materialization(definitions, fields),
            identity: is_identity(slots),
        }
    }
}

fn requires_materialization(definitions: &[ColumnDef], fields: &[String]) -> bool {
    #[cfg(test)]
    INSPECTIONS.with(|counts| counts.set((counts.get().0 + 1, counts.get().1)));
    crate::query::generated::projection_contains_virtual_generated_column(definitions, fields)
        || crate::query::document_projection::projections_use_tuple_xmin(fields, definitions)
}

fn is_identity(slots: &[ProjectedValueSlot]) -> bool {
    slots.iter().enumerate().all(|(expected, slot)| {
        #[cfg(test)]
        INSPECTIONS.with(|counts| counts.set((counts.get().0, counts.get().1 + 1)));
        matches!(slot, ProjectedValueSlot::Field(actual) if *actual == expected)
    })
}

#[cfg(test)]
thread_local! {
    static INSPECTIONS: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}

#[cfg(test)]
pub(super) fn inspections() -> (usize, usize) {
    INSPECTIONS.with(std::cell::Cell::get)
}
