//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One operator invocation keeps initialization until its first actual pull.

use crate::{ExecError, ExecResult};
use std::time::{Duration, Instant};
use uqa_core::io_usage::IOContext;

#[cfg(test)]
mod tests;

pub(super) type Clock = Option<fn() -> Instant>;

pub use uqa_sql::result::explain::ExplainNodeMetrics as NodeMetrics;

pub(super) fn accumulate(total: &mut NodeMetrics, other: NodeMetrics) -> ExecResult<()> {
    let next = NodeMetrics {
        rows: total.rows.checked_add(other.rows).ok_or_else(overflow)?,
        loops: total.loops.checked_add(other.loops).ok_or_else(overflow)?,
        startup: total
            .startup
            .checked_add(other.startup)
            .ok_or_else(overflow)?,
        total: total.total.checked_add(other.total).ok_or_else(overflow)?,
    };
    *total = next;
    Ok(())
}

#[derive(Clone, Copy, Default)]
pub(super) struct Cycle {
    consumed: bool,
    elapsed: Duration,
    rows: u64,
    first_output: Option<Duration>,
}

impl Cycle {
    pub(super) fn initialize(&mut self, elapsed: Duration) -> ExecResult<()> {
        self.elapsed = self.elapsed.checked_add(elapsed).ok_or_else(overflow)?;
        Ok(())
    }

    pub(super) fn consume(&mut self, step: Step) -> ExecResult<()> {
        let first_output = if self.first_output.is_none() {
            step.first_output
                .map(|startup| self.elapsed.checked_add(startup).ok_or_else(overflow))
                .transpose()?
        } else {
            self.first_output
        };
        let rows = self.rows.checked_add(step.rows).ok_or_else(overflow)?;
        let elapsed = self
            .elapsed
            .checked_add(step.elapsed)
            .ok_or_else(overflow)?;
        self.consumed = true;
        self.first_output = first_output;
        self.rows = rows;
        self.elapsed = elapsed;
        Ok(())
    }

    pub(super) fn snapshot(&self) -> NodeMetrics {
        let mut copy = *self;
        copy.finish()
    }

    pub(super) fn finish(&mut self) -> NodeMetrics {
        let completed = std::mem::take(self);
        if completed.consumed {
            NodeMetrics {
                rows: completed.rows,
                loops: 1,
                startup: completed.first_output.unwrap_or(completed.elapsed),
                total: completed.elapsed,
            }
        } else {
            NodeMetrics::default()
        }
    }
}

#[derive(Clone)]
pub(crate) struct Step {
    clock: Clock,
    started: Option<Instant>,
    paused: Duration,
    elapsed: Duration,
    first_output: Option<Duration>,
    rows: u64,
    failed: bool,
    io_parent: Option<IOContext>,
}

impl Step {
    pub(super) fn new(clock: Clock) -> Self {
        Self {
            clock,
            started: clock.map(|now| now()),
            paused: Duration::ZERO,
            elapsed: Duration::ZERO,
            first_output: None,
            rows: 0,
            failed: false,
            io_parent: None,
        }
    }

    pub(super) fn with_io_parent(mut self, parent: Option<IOContext>) -> Self {
        self.io_parent = parent;
        self
    }

    pub(super) fn ready(&mut self, rows: usize) -> ExecResult<()> {
        let result = (|| {
            let count = self
                .rows
                .checked_add(u64::try_from(rows).map_err(|_| overflow())?)
                .ok_or_else(overflow)?;
            let first_output = if self.first_output.is_none() && rows != 0 {
                Some(self.elapsed_now()?)
            } else {
                self.first_output
            };
            self.rows = count;
            self.first_output = first_output;
            Ok(())
        })();
        self.failed |= result.is_err();
        result
    }

    pub(super) fn downstream<T>(&mut self, call: impl FnOnce() -> ExecResult<T>) -> ExecResult<T> {
        let _io = self.io_parent.as_ref().map(IOContext::enter);
        let started = self.clock.map(|now| now());
        let result = call();
        let accounting = (|| {
            if let Some(started) = started {
                let elapsed = self.clock.expect("timed downstream")()
                    .checked_duration_since(started)
                    .ok_or_else(overflow)?;
                self.paused = self.paused.checked_add(elapsed).ok_or_else(overflow)?;
            }
            Ok(())
        })();
        self.failed |= accounting.is_err();
        crate::physical::with_cleanup(result, accounting, "account for EXPLAIN downstream time")
    }

    pub(super) fn finish(mut self) -> ExecResult<Self> {
        if self.failed {
            return Err(overflow());
        }
        self.elapsed = self.elapsed_now()?;
        self.io_parent = None;
        Ok(self)
    }

    pub(super) fn untimed(mut self) -> Self {
        self.elapsed = Duration::ZERO;
        self.first_output = self.first_output.map(|_| Duration::ZERO);
        self
    }

    pub(super) fn elapsed(&self) -> Duration {
        self.elapsed
    }

    fn elapsed_now(&self) -> ExecResult<Duration> {
        self.started.map_or(Ok(Duration::ZERO), |started| {
            self.clock.expect("timed step")()
                .checked_duration_since(started)
                .and_then(|elapsed| elapsed.checked_sub(self.paused))
                .ok_or_else(overflow)
        })
    }
}

fn overflow() -> ExecError {
    ExecError::Other("EXPLAIN operator measurements overflowed".into())
}
