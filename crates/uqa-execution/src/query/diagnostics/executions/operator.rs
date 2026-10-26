//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Forward every physical capability while measuring the request that actually invokes it.

use super::{instance::Instance, receivers::AggregateReceiver, Event, Step};
use crate::{
    physical::construction::OperatorBuild, query::diagnostics::QueryDiagnostics,
    BackwardScanSupport, Batch, ExecResult, PhysicalOperator, PhysicalOrder, PhysicalScanDirection,
    RowSchema,
};
use std::{cell::RefCell, num::NonZeroUsize};
use uqa_sql::SQLError;

struct ObservedOperator<'a> {
    input: Box<dyn PhysicalOperator + 'a>,
    instance: Instance<'a>,
}

impl QueryDiagnostics {
    pub(crate) fn observe<'a>(
        &'a self,
        input: Box<dyn PhysicalOperator + 'a>,
        kind: &'static str,
    ) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
        let (input, instance) = OperatorBuild::run(input, |_| {
            self.instance(kind, size_of::<ObservedOperator<'_>>())
        })?;
        Ok(match instance {
            Some(instance) => Box::new(ObservedOperator { input, instance }),
            None => input,
        })
    }
}

impl ObservedOperator<'_> {
    fn pull(
        &mut self,
        call: impl FnOnce(&mut dyn PhysicalOperator) -> ExecResult<Option<Batch>>,
    ) -> ExecResult<Option<Batch>> {
        self.instance.perform(
            self.input.as_mut(),
            |input, step| {
                let batch = call(input)?;
                if let (Some(step), Some(batch)) = (step, &batch) {
                    step.ready(batch.rows.len())?;
                }
                Ok(batch)
            },
            |_, step| Event::Consume(step),
            None,
        )
    }
}

impl PhysicalOperator for ObservedOperator<'_> {
    fn row_schema(&self) -> &RowSchema {
        self.input.row_schema()
    }
    fn schema(&self) -> &[String] {
        self.input.schema()
    }
    fn estimated_cardinality(&self) -> Option<u64> {
        self.input.estimated_cardinality()
    }
    fn output_ordering(&self) -> &[PhysicalOrder] {
        self.input.output_ordering()
    }
    fn backward_scan_support(&self) -> BackwardScanSupport {
        self.input.backward_scan_support()
    }
    fn supports_batched_consumption(&self) -> bool {
        self.input.supports_batched_consumption()
    }

    fn open(&mut self) -> ExecResult<()> {
        self.instance.perform(
            self.input.as_mut(),
            |input, _| input.open(),
            |_, step| Event::Initialize(step),
            None,
        )
    }
    fn next(&mut self) -> ExecResult<Option<Batch>> {
        self.pull(|input| input.next())
    }
    fn next_with_demand(&mut self, rows: NonZeroUsize) -> ExecResult<Option<Batch>> {
        self.pull(|input| input.next_with_demand(rows))
    }
    fn next_direction(&mut self, direction: PhysicalScanDirection) -> ExecResult<Option<Batch>> {
        self.pull(|input| input.next_direction(direction))
    }
    fn rewind(&mut self) -> ExecResult<()> {
        self.instance.perform(
            self.input.as_mut(),
            |input, _| input.rewind(),
            |result, step| {
                if result.is_ok() {
                    Event::Rewind(step)
                } else {
                    Event::Initialize(step)
                }
            },
            None,
        )
    }
    fn close(&mut self) -> ExecResult<()> {
        self.instance.perform(
            self.input.as_mut(),
            |input, _| input.close(),
            |_, step| Event::Close(step),
            Some(PhysicalOperator::close),
        )
    }
    fn consume_into_aggregate(
        &mut self,
        executor: &mut dyn crate::relational::AggregateExecutor,
    ) -> ExecResult<bool> {
        self.instance.perform(
            self.input.as_mut(),
            |input, step| match step {
                Some(step) => input.consume_into_aggregate(&mut AggregateReceiver {
                    inner: executor,
                    step,
                }),
                None => input.consume_into_aggregate(executor),
            },
            probe_event,
            None,
        )
    }
    fn collect_materialized(
        &mut self,
        sink: &mut crate::query::output::MaterializedResultSink<'_>,
    ) -> ExecResult<bool> {
        self.instance.perform(
            self.input.as_mut(),
            |input, step| {
                let Some(step) = step else {
                    return input.collect_materialized(sink);
                };
                let observed = RefCell::new(*step);
                let result = sink.observe(&observed, |sink| input.collect_materialized(sink));
                *step = observed.into_inner();
                result
            },
            probe_event,
            None,
        )
    }
}

fn probe_event(result: &ExecResult<bool>, step: Step) -> Event {
    if matches!(result, Ok(false)) {
        Event::Initialize(step)
    } else {
        Event::Consume(step)
    }
}

#[cfg(test)]
mod tests;
