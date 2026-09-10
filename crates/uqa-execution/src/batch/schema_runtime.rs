//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Runtime operations on SQL-owned schema layouts.

use super::{ExecResult, PhysicalRow, PhysicalRowView, RowSchema};

/// Physical row operations implemented by the execution crate.
pub trait RowSchemaExecution {
    fn view<'a>(&'a self, row: &'a PhysicalRow) -> PhysicalRowView<'a>;

    fn relayout_physical_row(
        &self,
        row: PhysicalRow,
        target: &RowSchema,
    ) -> ExecResult<PhysicalRow>;
}

impl RowSchemaExecution for RowSchema {
    fn view<'a>(&'a self, row: &'a PhysicalRow) -> PhysicalRowView<'a> {
        PhysicalRowView { schema: self, row }
    }

    fn relayout_physical_row(
        &self,
        row: PhysicalRow,
        target: &RowSchema,
    ) -> ExecResult<PhysicalRow> {
        let slots = self.relayout_slots(target)?;
        Ok(row.project_slots(&slots))
    }
}
