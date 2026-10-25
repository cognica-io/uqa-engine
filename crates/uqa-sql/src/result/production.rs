//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Admitted public result metadata and named rows consume already-produced positional cells.

use super::{ColumnType, ResultRow, SQLResult, SQLResultKind};
use crate::SQLError;
use uqa_core::{
    memory::{MemoryReservation, Produced, ProductionControl, ProductionVec},
    Value,
};

// Completed rows and remaining cells are destroyed before their shared input lease on every failure path.
struct Rows<'a> {
    output: ProductionVec<'a, ResultRow>,
    remaining: std::vec::IntoIter<Value>,
    cells: Option<MemoryReservation>,
}

impl SQLResult {
    /// Construct a single-column result under the cells' original allowance and cancellation owners.
    ///
    /// Column names, type metadata, vector capacities and live map entries acquire their leases before construction. Map accounting follows Core's exposed-payload contract: private B-tree node slack and allocator bookkeeping are excluded. The returned owner retains the admitted cells and metadata until an explicit public API handoff.
    pub fn from_column_with_control(
        name: &str,
        ty: &ColumnType,
        cells: Produced<Vec<Value>>,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        let mut columns = ProductionVec::new(*control);
        columns.push_produced(control.copy_text(name)?)?;
        let mut column_types = ProductionVec::new(*control);
        let (ty, memory) = ty.clone_with_control(control)?.into_parts();
        column_types.push_produced(control.finish(Some(ty), memory)?)?;
        let (cells, cell_memory) = cells.into_parts();
        let mut rows = Rows {
            output: ProductionVec::new(*control),
            remaining: cells.into_iter(),
            cells: cell_memory,
        };
        for cell in rows.remaining.by_ref() {
            let entry = control.reserve(size_of::<(String, Value)>())?;
            let (key, key_memory) = control.copy_text(name)?.into_parts();
            let row = ResultRow::from([(key, cell)]);
            rows.output
                .push_produced(control.finish(row, control.combine(entry, key_memory))?)?;
        }
        let columns = columns.finish()?;
        let column_types = column_types.finish()?;
        let output = rows.output.finish()?;
        drop(rows.remaining);
        let (columns, column_memory) = columns.into_parts();
        let (column_types, type_memory) = column_types.into_parts();
        let (output, row_memory) = output.into_parts();
        let memory = control.combine(
            control.combine(rows.cells, row_memory),
            control.combine(column_memory, type_memory),
        );
        Ok(control.finish(
            Self {
                kind: SQLResultKind::Rows,
                command_tag: None,
                columns,
                column_types,
                row_schema: None,
                rows: output,
                positional_rows: None,
                affected_rows: 0,
            },
            memory,
        )?)
    }
}

#[cfg(test)]
mod tests;
