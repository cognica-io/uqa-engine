//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A retained instance records only the request that is currently executing it.

use super::{account, io::Operation, Event, Step};
use crate::{
    physical::with_cleanup,
    query::diagnostics::{Failure, QueryDiagnostics},
    ExecResult,
};
use uqa_core::memory::MemoryReservation;
use uqa_sql::SQLError;

pub(super) struct Instance<'a> {
    diagnostics: &'a QueryDiagnostics,
    id: u64,
    kind: &'static str,
    _memory: MemoryReservation,
}

impl QueryDiagnostics {
    pub(super) fn instance(
        &self,
        kind: &'static str,
        bytes: usize,
    ) -> Result<Option<Instance<'_>>, SQLError> {
        let collector = self
            .capture()
            .filter(|collector| collector.has_execution_scope());
        let control = collector
            .as_ref()
            .map(|collector| collector.control().clone())
            .or_else(|| self.request_control());
        let Some(control) = control else {
            return Ok(None);
        };
        control.cancellation().check().inspect_err(|_| {
            if let Some(collector) = &collector {
                collector.fail_execution(Failure::Cancelled);
            }
        })?;
        let id = self.allocate_instance().inspect_err(|_| {
            if let Some(collector) = &collector {
                collector.fail_execution(Failure::Measurement);
            }
        })?;
        let memory = control.memory().reserve(bytes).inspect_err(|_| {
            if let Some(collector) = &collector {
                collector.fail_execution(Failure::ExecutionMemory);
            }
        })?;
        if let Some(collector) = &collector {
            collector.register_execution(id, kind)?;
        }
        Ok(Some(Instance {
            diagnostics: self,
            id,
            kind,
            _memory: memory,
        }))
    }
}

impl Instance<'_> {
    pub(super) fn perform<I: ?Sized, T>(
        &self,
        input: &mut I,
        call: impl FnOnce(&mut I, Option<&mut Step>) -> ExecResult<T>,
        event: impl FnOnce(&ExecResult<T>, Step) -> Event,
        cleanup: Option<fn(&mut I) -> ExecResult<()>>,
    ) -> ExecResult<T> {
        let Some(collector) = self
            .diagnostics
            .capture()
            .filter(|collector| collector.has_execution_scope())
        else {
            return call(input, None);
        };
        let admission = (|| {
            let timing = collector.register_execution(self.id, self.kind)?;
            let parent = self.diagnostics.io_context();
            let io = Operation::enter(&parent, collector.control().memory())?;
            Ok::<_, SQLError>((timing, io.as_ref().map(|_| parent), io))
        })();
        let (timing, parent, io) = match admission {
            Ok(admitted) => admitted,
            Err(error) => {
                collector.fail_execution(match error.sqlstate() {
                    Some("53200") => Failure::ExecutionMemory,
                    Some("57014") => Failure::Cancelled,
                    _ => Failure::Measurement,
                });
                return match cleanup {
                    Some(close) => with_cleanup(
                        Err(error.into()),
                        close(input),
                        "close input after measurement admission failure",
                    ),
                    None => Err(error.into()),
                };
            }
        };
        let mut step = Step::new(timing.then_some(std::time::Instant::now)).with_io_parent(parent);
        let result = call(input, Some(&mut step));
        let accounting = match step.finish() {
            Ok(step) => account(&collector, self.id, event(&result, step), io.as_ref()),
            Err(error) => {
                collector.fail_execution(Failure::Measurement);
                Err(error)
            }
        };
        with_cleanup(result, accounting, "record native operator measurements")
    }
}
