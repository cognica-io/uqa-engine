//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Statement-owned CTE rows: completed command/recursive buffers or one demand-driven read-only producer.

use crate::physical::{physical_exec_error, with_cleanup};
use crate::spill::BufferedIndexedSpill;
use crate::{Batch, ExecError, ExecResult, PhysicalOperator, RowSchema, SharedSpill};
use parking_lot::Mutex;
use std::{num::NonZeroUsize, sync::Arc};
use uqa_core::memory::{MemoryReservation, ProductionControl};
use uqa_sql::SQLError;

#[derive(Clone)]
pub enum CteRows {
    Complete(SharedSpill),
    Prefix(SharedPrefix),
}
impl CteRows {
    pub fn row_schema(&self) -> &RowSchema {
        match self {
            Self::Complete(rows) => rows.row_schema(),
            Self::Prefix(rows) => &rows.schema,
        }
    }
    pub fn into_operator(self) -> Box<dyn PhysicalOperator> {
        match self {
            Self::Complete(rows) => Box::new(crate::SharedSpillScan::new(rows)),
            Self::Prefix(rows) => Box::new(PrefixScan { rows, position: 0 }),
        }
    }
    #[cfg(test)]
    pub(crate) fn from_producer(input: Box<dyn PhysicalOperator>, budget: usize) -> Self {
        Self::from_producer_with_control(input, budget, &ProductionControl::uncontrolled())
            .expect("ordinary CTE producer")
    }

    pub(crate) fn from_producer_with_control(
        input: Box<dyn PhysicalOperator>,
        budget: usize,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let (input, memory) = crate::physical::construction::OperatorBuild::run(input, |_| {
            Ok(control.reserve(size_of::<Mutex<PrefixState>>())?)
        })?;
        let schema = input.row_schema().clone();
        Ok(Self::Prefix(SharedPrefix {
            schema: schema.clone(),
            state: Arc::new(Mutex::new(PrefixState {
                rows: BufferedIndexedSpill::new(schema, budget),
                input: Some(input),
                opened: false,
                failure: None,
                _memory: memory,
            })),
        }))
    }
}

#[derive(Clone)]
pub struct SharedPrefix {
    schema: RowSchema,
    state: Arc<Mutex<PrefixState>>,
}
struct PrefixState {
    rows: BufferedIndexedSpill,
    input: Option<Box<dyn PhysicalOperator>>,
    opened: bool,
    failure: Option<SQLError>,
    _memory: Option<MemoryReservation>,
}
impl PrefixState {
    fn finish(&mut self) -> ExecResult<()> {
        self.opened = false;
        self.input.take().map_or(Ok(()), |mut input| input.close())
    }
    fn extend(&mut self, demand: NonZeroUsize) -> ExecResult<()> {
        if let Some(error) = &self.failure {
            return Err(ExecError::SQL(error.clone()));
        }
        let result = self.extend_input(demand);
        if let Err(error) = result {
            let error =
                with_cleanup::<()>(Err(error), self.finish(), "close CTE producer").unwrap_err();
            let error = physical_exec_error(error);
            self.failure = Some(error.clone());
            return Err(ExecError::SQL(error));
        }
        Ok(())
    }
    fn extend_input(&mut self, demand: NonZeroUsize) -> ExecResult<()> {
        let Some(input) = self.input.as_mut() else {
            return Ok(());
        };
        if !self.opened {
            self.opened = true;
            input.open()?;
        }
        let Some(batch) = input.next_with_demand(demand)? else {
            return self.finish();
        };
        if &batch.schema != self.rows.row_schema() {
            return Err(ExecError::Other(
                "CTE producer changed its row schema".into(),
            ));
        }
        for row in batch.rows {
            self.rows.push(&row)?;
        }
        Ok(())
    }
}

impl SharedPrefix {
    /// End the producer at its owning query boundary, independently of how many readers still retain its cached rows.
    pub(crate) fn close(&self) -> ExecResult<()> {
        let mut state = self.state.lock();
        state.finish().map_err(|error| {
            let error = physical_exec_error(error);
            state.failure = Some(error.clone());
            ExecError::SQL(error)
        })
    }
}
impl Drop for PrefixState {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

struct PrefixScan {
    rows: SharedPrefix,
    position: u64,
}
impl PrefixScan {
    fn pull(&mut self, demand: NonZeroUsize) -> ExecResult<Option<Batch>> {
        let mut state = self.rows.state.lock();
        while self.position == state.rows.len()
            && (state.input.is_some() || state.failure.is_some())
        {
            state.extend(demand)?;
        }
        if self.position == state.rows.len() {
            return Ok(None);
        }
        let count = (state.rows.len() - self.position).min(demand.get() as u64);
        let mut rows = Vec::with_capacity(count as usize);
        for _ in 0..count {
            rows.push(state.rows.get(self.position)?);
            self.position += 1;
        }
        Ok(Some(Batch::from_physical_rows(
            self.rows.schema.clone(),
            rows,
        )))
    }
}
impl PhysicalOperator for PrefixScan {
    fn backward_scan_support(&self) -> crate::BackwardScanSupport {
        crate::BackwardScanSupport::Materialize
    }
    fn row_schema(&self) -> &RowSchema {
        &self.rows.schema
    }
    fn open(&mut self) -> ExecResult<()> {
        self.position = 0;
        Ok(())
    }
    fn next(&mut self) -> ExecResult<Option<Batch>> {
        self.pull(NonZeroUsize::new(crate::DEFAULT_BATCH_SIZE).expect("nonzero batch size"))
    }
    fn next_with_demand(&mut self, rows: NonZeroUsize) -> ExecResult<Option<Batch>> {
        self.pull(rows)
    }
    fn close(&mut self) -> ExecResult<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
