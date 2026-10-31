//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Keep local CTE producers until the retained physical query closes.

use super::CteProducers;
use crate::physical::with_cleanup;
use crate::{Batch, ExecResult, PhysicalOperator, RowSchema};
use std::num::NonZeroUsize;

pub(super) struct ProducerQuery<'a> {
    input: Box<dyn PhysicalOperator + 'a>,
    producers: Option<CteProducers>,
    _memory: Option<uqa_core::memory::MemoryReservation>,
}

impl<'a> ProducerQuery<'a> {
    pub(super) fn with_control(
        input: Box<dyn PhysicalOperator + 'a>,
        producers: CteProducers,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Box<Self>, uqa_sql::SQLError> {
        let (input, memory) = match crate::physical::construction::OperatorBuild::run(input, |_| {
            Ok(control.reserve(size_of::<Self>())?)
        }) {
            Ok(prepared) => prepared,
            Err(error) => return producers.finish(Err(error)),
        };
        Ok(Box::new(Self {
            input,
            producers: Some(producers),
            _memory: memory,
        }))
    }
}

impl PhysicalOperator for ProducerQuery<'_> {
    fn row_schema(&self) -> &RowSchema {
        self.input.row_schema()
    }
    fn estimated_cardinality(&self) -> Option<u64> {
        self.input.estimated_cardinality()
    }
    fn output_ordering(&self) -> &[crate::PhysicalOrder] {
        self.input.output_ordering()
    }
    fn backward_scan_support(&self) -> crate::BackwardScanSupport {
        self.input.backward_scan_support()
    }
    fn supports_batched_consumption(&self) -> bool {
        self.input.supports_batched_consumption()
    }
    fn consume_into_aggregate(
        &mut self,
        executor: &mut dyn crate::relational::AggregateExecutor,
    ) -> ExecResult<bool> {
        self.input.consume_into_aggregate(executor)
    }
    fn open(&mut self) -> ExecResult<()> {
        self.input.open()
    }
    fn next(&mut self) -> ExecResult<Option<Batch>> {
        self.input.next()
    }
    fn next_with_demand(&mut self, rows: NonZeroUsize) -> ExecResult<Option<Batch>> {
        self.input.next_with_demand(rows)
    }
    fn next_direction(
        &mut self,
        direction: crate::PhysicalScanDirection,
    ) -> ExecResult<Option<Batch>> {
        self.input.next_direction(direction)
    }
    fn rewind(&mut self) -> ExecResult<()> {
        self.input.rewind()
    }
    fn close(&mut self) -> ExecResult<()> {
        let Some(producers) = self.producers.take() else {
            return Ok(());
        };
        with_cleanup(
            self.input.close(),
            producers.close(),
            "close query CTE producers",
        )
    }
}

impl Drop for ProducerQuery<'_> {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
