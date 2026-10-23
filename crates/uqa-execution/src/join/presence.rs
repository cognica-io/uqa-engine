//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A nullable join input retains row presence independently of its field values.

use crate::{
    BackwardScanSupport, Batch, ExecError, ExecResult, OwnedPhysicalRow, PhysicalOperator,
    PhysicalOrder, PhysicalRow, PhysicalScanDirection, RowSchema, RowSchemaExecution,
};
use uqa_core::Value;
use uqa_sql::ast::InternalRelationId;

pub(crate) struct NullableRow {
    input: RowSchema,
    pub(crate) schema: RowSchema,
    marker: Option<PhysicalRow>,
}

impl NullableRow {
    pub(crate) fn new(input: &RowSchema, nullable: bool) -> Self {
        let marked = if nullable {
            input.with_nullable_relations(InternalRelationId::allocate().column(0))
        } else {
            None
        };
        Self {
            input: input.clone(),
            schema: marked.as_ref().unwrap_or(input).clone(),
            // Every produced row shares this immutable fragment. NULL extension uses the ordinary physical NULL row of the marked width.
            marker: marked.map(|_| PhysicalRow::from_values(vec![Value::Bool(true)])),
        }
    }

    pub(crate) fn row(&self, row: PhysicalRow) -> PhysicalRow {
        match &self.marker {
            Some(marker) => PhysicalRow::concat_left_owned(row, marker),
            None => row,
        }
    }

    pub(crate) fn owned_row(&self, row: OwnedPhysicalRow) -> ExecResult<PhysicalRow> {
        let physical = if self.input.shares_index(&row.schema)
            || self.input.has_same_physical_layout(&row.schema)
        {
            row.row
        } else {
            row.schema.relayout_physical_row(row.row, &self.input)?
        };
        Ok(self.row(physical))
    }

    fn batch(&self, batch: Batch) -> ExecResult<Batch> {
        if !self.input.shares_index(&batch.schema)
            && !self.input.has_same_physical_layout(&batch.schema)
        {
            return Err(ExecError::Other(
                "nullable input changed its physical schema".into(),
            ));
        }
        Ok(Batch::from_physical_rows(
            self.schema.clone(),
            batch.rows.into_iter().map(|row| self.row(row)).collect(),
        ))
    }
}

pub(crate) fn input<'a>(
    child: Box<dyn PhysicalOperator + 'a>,
    nullable: bool,
) -> Box<dyn PhysicalOperator + 'a> {
    let presence = NullableRow::new(child.row_schema(), nullable);
    if presence.marker.is_none() {
        child
    } else {
        Box::new(NullableInput { child, presence })
    }
}

struct NullableInput<'a> {
    child: Box<dyn PhysicalOperator + 'a>,
    presence: NullableRow,
}

impl PhysicalOperator for NullableInput<'_> {
    fn row_schema(&self) -> &RowSchema {
        &self.presence.schema
    }
    fn estimated_cardinality(&self) -> Option<u64> {
        self.child.estimated_cardinality()
    }
    fn output_ordering(&self) -> &[PhysicalOrder] {
        self.child.output_ordering()
    }
    fn backward_scan_support(&self) -> BackwardScanSupport {
        self.child.backward_scan_support()
    }
    fn open(&mut self) -> ExecResult<()> {
        self.child.open()
    }
    fn next(&mut self) -> ExecResult<Option<Batch>> {
        self.child
            .next()?
            .map(|batch| self.presence.batch(batch))
            .transpose()
    }

    fn next_with_demand(&mut self, rows: std::num::NonZeroUsize) -> ExecResult<Option<Batch>> {
        self.child
            .next_with_demand(rows)?
            .map(|batch| self.presence.batch(batch))
            .transpose()
    }
    fn next_direction(&mut self, direction: PhysicalScanDirection) -> ExecResult<Option<Batch>> {
        self.child
            .next_direction(direction)?
            .map(|batch| self.presence.batch(batch))
            .transpose()
    }
    fn rewind(&mut self) -> ExecResult<()> {
        self.child.rewind()
    }
    fn close(&mut self) -> ExecResult<()> {
        self.child.close()
    }
}

#[cfg(test)]
mod tests;
