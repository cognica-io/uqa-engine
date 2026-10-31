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
use uqa_core::memory::{MemoryReservation, ProductionControl};
use uqa_sql::ast::InternalRelationId;
use uqa_sql::SQLError;

pub(crate) struct NullableRow {
    input: RowSchema,
    pub(crate) schema: RowSchema,
    marker: Option<PhysicalRow>,
}

impl NullableRow {
    #[cfg(test)]
    pub(crate) fn new(input: &RowSchema, nullable: bool) -> Self {
        Self::with_control(input, nullable, &ProductionControl::uncontrolled())
            .expect("ordinary nullable input")
    }

    pub(crate) fn with_control(
        input: &RowSchema,
        nullable: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let marked = if nullable {
            input.with_nullable_relations_with_control(
                InternalRelationId::allocate().column(0),
                control,
            )?
        } else {
            None
        };
        Ok(Self {
            input: input.clone(),
            schema: marked.as_ref().unwrap_or(input).clone(),
            marker: marked.map(|_| PhysicalRow::present()),
        })
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

pub(crate) fn input_with_control<'a>(
    child: Box<dyn PhysicalOperator + 'a>,
    nullable: bool,
    control: &ProductionControl<'_>,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    crate::physical::construction::OperatorBuild::build(child, |child| {
        let presence = NullableRow::with_control(child.row_schema(), nullable, control)?;
        if presence.marker.is_some() {
            let memory = control.reserve(size_of::<NullableInput<'_>>())?;
            child.wrap(|child| {
                Box::new(NullableInput {
                    child,
                    presence,
                    _memory: memory,
                })
            });
        }
        Ok(())
    })
}

pub(super) fn join_inputs<'a>(
    left: &mut crate::physical::construction::OperatorBuild<'a>,
    right: &mut crate::physical::construction::OperatorBuild<'a>,
    kind: uqa_sql::ast::JoinKind,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    use uqa_sql::ast::JoinKind;
    left.attach(|input| {
        input_with_control(
            input,
            matches!(kind, JoinKind::Right | JoinKind::Full),
            control,
        )
    })?;
    right.attach(|input| {
        input_with_control(
            input,
            matches!(kind, JoinKind::Left | JoinKind::Full),
            control,
        )
    })
}

struct NullableInput<'a> {
    child: Box<dyn PhysicalOperator + 'a>,
    presence: NullableRow,
    _memory: Option<MemoryReservation>,
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
