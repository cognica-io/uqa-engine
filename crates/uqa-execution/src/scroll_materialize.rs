//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Incremental directional materialization for scrollable executor boundaries.

use std::collections::VecDeque;

use crate::{
    BackwardScanSupport, Batch, ExecError, ExecResult, IndexedSpill, PhysicalOperator,
    PhysicalOrder, PhysicalRow, PhysicalScanDirection, RowSchema,
};

#[derive(Clone, Copy)]
enum MaterializePosition {
    BeforeFirst,
    OnRow(u64),
    AfterLast,
}

/// Cache an operator's output as it is first pulled forward, then expose the cached rows in either direction. This is the physical equivalent of `PostgreSQL`'s `Material` node: expressions below the boundary run once, while expressions in parents run again when a row is revisited.
pub struct ScrollMaterialize<'a> {
    child: Box<dyn PhysicalOperator + 'a>,
    schema: RowSchema,
    ordering: Vec<PhysicalOrder>,
    rows: Option<IndexedSpill>,
    pending: VecDeque<PhysicalRow>,
    position: MaterializePosition,
    eof: bool,
}

impl<'a> ScrollMaterialize<'a> {
    pub fn new(child: Box<dyn PhysicalOperator + 'a>) -> Self {
        let schema = child.row_schema().clone();
        let ordering = child.output_ordering().to_vec();
        Self {
            child,
            schema,
            ordering,
            rows: None,
            pending: VecDeque::new(),
            position: MaterializePosition::BeforeFirst,
            eof: false,
        }
    }

    fn rows_mut(&mut self) -> ExecResult<&mut IndexedSpill> {
        self.rows
            .as_mut()
            .ok_or_else(|| ExecError::Other("scroll materialization is not open".into()))
    }

    fn pull_child_row(&mut self) -> ExecResult<Option<PhysicalRow>> {
        loop {
            if let Some(row) = self.pending.pop_front() {
                return Ok(Some(row));
            }
            let Some(batch) = self.child.next()? else {
                return Ok(None);
            };
            if batch.schema != self.schema {
                return Err(ExecError::Other(format!(
                    "scroll materialization input schema mismatch: expected {:?}, got {:?}",
                    self.schema, batch.schema
                )));
            }
            self.pending.extend(batch.rows);
        }
    }

    fn row_at(&mut self, index: u64) -> ExecResult<Batch> {
        let row = self.rows_mut()?.get(index)?;
        Ok(Batch::from_physical_rows(self.schema.clone(), vec![row]))
    }

    fn next_forward(&mut self) -> ExecResult<Option<Batch>> {
        let target = match self.position {
            MaterializePosition::BeforeFirst => 0,
            MaterializePosition::OnRow(position) => position
                .checked_add(1)
                .ok_or_else(|| ExecError::Other("scroll position overflow".into()))?,
            MaterializePosition::AfterLast => return Ok(None),
        };
        if target < self.rows_mut()?.len() {
            self.position = MaterializePosition::OnRow(target);
            return self.row_at(target).map(Some);
        }
        if self.eof {
            self.position = MaterializePosition::AfterLast;
            return Ok(None);
        }
        let Some(row) = self.pull_child_row()? else {
            self.eof = true;
            self.position = MaterializePosition::AfterLast;
            return Ok(None);
        };
        self.rows_mut()?.push(&row)?;
        self.position = MaterializePosition::OnRow(target);
        Ok(Some(Batch::from_physical_rows(
            self.schema.clone(),
            vec![row],
        )))
    }

    fn next_backward(&mut self) -> ExecResult<Option<Batch>> {
        let target = match self.position {
            MaterializePosition::BeforeFirst => return Ok(None),
            MaterializePosition::OnRow(0) => {
                self.position = MaterializePosition::BeforeFirst;
                return Ok(None);
            }
            MaterializePosition::OnRow(position) => position - 1,
            MaterializePosition::AfterLast => {
                let row_count = self.rows_mut()?.len();
                let Some(position) = row_count.checked_sub(1) else {
                    self.position = MaterializePosition::BeforeFirst;
                    return Ok(None);
                };
                position
            }
        };
        self.position = MaterializePosition::OnRow(target);
        self.row_at(target).map(Some)
    }
}

impl PhysicalOperator for ScrollMaterialize<'_> {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }

    fn estimated_cardinality(&self) -> Option<u64> {
        self.child.estimated_cardinality()
    }

    fn output_ordering(&self) -> &[PhysicalOrder] {
        &self.ordering
    }

    fn backward_scan_support(&self) -> BackwardScanSupport {
        BackwardScanSupport::Native
    }

    fn open(&mut self) -> ExecResult<()> {
        self.pending.clear();
        self.position = MaterializePosition::BeforeFirst;
        self.eof = false;
        self.rows = Some(IndexedSpill::new(self.schema.clone())?);
        self.child.open()
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        self.next_forward()
    }

    fn next_direction(&mut self, direction: PhysicalScanDirection) -> ExecResult<Option<Batch>> {
        match direction {
            PhysicalScanDirection::Forward => self.next_forward(),
            PhysicalScanDirection::Backward => self.next_backward(),
        }
    }

    fn rewind(&mut self) -> ExecResult<()> {
        self.position = MaterializePosition::BeforeFirst;
        Ok(())
    }

    fn close(&mut self) -> ExecResult<()> {
        self.pending.clear();
        self.rows = None;
        self.position = MaterializePosition::BeforeFirst;
        self.eof = true;
        self.child.close()
    }
}

/// Materialize an operator only when it identifies its output as a safe semantic boundary.
pub fn prepare_backward_scan<'a>(
    operator: Box<dyn PhysicalOperator + 'a>,
) -> Box<dyn PhysicalOperator + 'a> {
    if operator.backward_scan_support() == BackwardScanSupport::Materialize {
        Box::new(ScrollMaterialize::new(operator))
    } else {
        operator
    }
}
