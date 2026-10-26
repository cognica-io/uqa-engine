//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Invocation-owned observations of actual native physical instances.

use super::{io, CapturedDiagnostics, Failure, InvocationDiagnostics};
use crate::{physical::physical_exec_error, ExecResult};
use uqa_core::{
    io_usage::IOUsage,
    memory::{BudgetedMap, BudgetedVec},
};
use uqa_sql::{result::explain::ExplainNodeExecution, SQLError};
use uqa_storage::read_control::StorageReadControl;

mod cycle;
mod instance;
#[cfg(test)]
mod io_tests;
mod operator;
mod receivers;
mod rows;
pub(crate) use cycle::Step;
use cycle::{accumulate, Cycle, NodeMetrics};
pub(crate) use receivers::MaterializedObservation;

pub(super) struct Executions {
    timing: bool,
    nodes: BudgetedMap<u64, Record>,
}

struct Record {
    kind: &'static str,
    io: IOUsage,
    completed: NodeMetrics,
    running: Cycle,
}

#[derive(Clone)]
pub(super) enum Event {
    Initialize(Step),
    Consume(Step),
    Rewind(Step),
    Close(Step),
}

impl Executions {
    pub(super) fn new(control: &StorageReadControl, timing: bool) -> Self {
        Self {
            timing,
            nodes: BudgetedMap::new(control.memory()),
        }
    }

    fn register(&mut self, id: u64, kind: &'static str) -> Result<(), SQLError> {
        if !self.nodes.contains_key(&id) {
            self.nodes.insert(
                id,
                Record {
                    kind,
                    io: IOUsage::default(),
                    completed: NodeMetrics::default(),
                    running: Cycle::default(),
                },
            )?;
        }
        Ok(())
    }

    fn record(&mut self, id: u64, event: &Event, io: IOUsage) -> ExecResult<()> {
        let record = self
            .nodes
            .get_mut(&id)
            .expect("operator was admitted before execution");
        let io = record.io.checked_add(io).ok_or_else(|| {
            crate::ExecError::Other("EXPLAIN operator I/O measurements overflowed".into())
        })?;
        let elapsed = |step: &Step| {
            if self.timing {
                step.elapsed()
            } else {
                std::time::Duration::ZERO
            }
        };
        let measured = match event {
            Event::Initialize(step) => record.running.initialize(elapsed(step)),
            Event::Consume(step) => record.running.consume(if self.timing {
                step.clone()
            } else {
                step.clone().untimed()
            }),
            Event::Close(step) => {
                record.running.initialize(elapsed(step))?;
                accumulate(&mut record.completed, record.running.finish())
            }
            Event::Rewind(step) => {
                accumulate(&mut record.completed, record.running.finish())?;
                record.running.initialize(elapsed(step))
            }
        };
        measured?;
        record.io = io;
        Ok(())
    }

    pub(super) fn snapshot(
        &self,
        control: &StorageReadControl,
    ) -> Result<BudgetedVec<ExplainNodeExecution>, SQLError> {
        let mut nodes = BudgetedVec::new(control.memory());
        for (_, record) in self.nodes.iter() {
            control.cancellation().check()?;
            let mut metrics = record.completed;
            accumulate(&mut metrics, record.running.snapshot()).map_err(physical_exec_error)?;
            nodes.push(ExplainNodeExecution {
                kind: record.kind,
                io: record.io,
                metrics,
            })?;
        }
        Ok(nodes)
    }
}

impl InvocationDiagnostics {
    pub(super) fn has_execution_scope(&self) -> bool {
        let mut current = Some(self);
        while let Some(collector) = current {
            let records = collector.records.lock();
            if records.closed {
                return false;
            }
            if records.executions.is_some() {
                return true;
            }
            current = collector.parent.as_ref().map(|parent| &***parent);
        }
        false
    }

    pub(super) fn register_execution(&self, id: u64, kind: &'static str) -> Result<bool, SQLError> {
        let mut timing = false;
        self.visit_executions(|nodes| {
            nodes.register(id, kind)?;
            timing |= nodes.timing;
            Ok(())
        })?;
        Ok(timing)
    }

    pub(super) fn record_execution(&self, id: u64, event: Event, io: IOUsage) -> ExecResult<()> {
        self.visit_executions(|nodes| nodes.record(id, &event, io).map_err(physical_exec_error))?;
        Ok(())
    }

    fn visit_executions(
        &self,
        mut apply: impl FnMut(&mut Executions) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        let result: Result<(), SQLError> = (|| {
            let mut current = Some(self);
            while let Some(collector) = current {
                let mut records = collector.records.lock();
                if records.closed {
                    break;
                }
                if let Some(nodes) = &mut records.executions {
                    collector.control.cancellation().check()?;
                    apply(nodes)?;
                }
                current = collector.parent.as_ref().map(|parent| &***parent);
            }
            Ok(())
        })();
        if let Err(error) = &result {
            let failure = match error.sqlstate() {
                Some("57014") => Failure::Cancelled,
                Some("53200") => Failure::ExecutionMemory,
                _ => Failure::Measurement,
            };
            self.fail_execution(failure);
        }
        result
    }

    pub(super) fn fail_execution(&self, failure: Failure) {
        let mut current = Some(self);
        while let Some(collector) = current {
            let mut records = collector.records.lock();
            if records.closed {
                break;
            }
            records.failed.get_or_insert(failure);
            current = collector.parent.as_ref().map(|parent| &***parent);
        }
    }
}

fn account(
    collector: &CapturedDiagnostics,
    id: u64,
    event: Event,
    io: Option<&io::Operation>,
) -> ExecResult<()> {
    let usage = io
        .map_or(Ok(IOUsage::default()), io::Operation::finish)
        .inspect_err(|_| collector.fail_execution(Failure::Measurement))?;
    collector.record_execution(id, event, usage)
}
