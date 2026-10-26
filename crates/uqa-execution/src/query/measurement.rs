//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Measure actual output production without including the receiving sink's work.

use std::{
    cell::Cell,
    time::{Duration, Instant},
};
use uqa_sql::{result::explain::ExplainNodeMetrics, SQLError};

#[cfg(test)]
mod tests;

pub struct QueryMeasurement {
    clock: Option<fn() -> Instant>,
    running: Cell<Option<Running>>,
    completed: Cell<ExplainNodeMetrics>,
}

/// A relation destination executes EXPLAIN's selected query with its original measurement owner, while retaining its separate declared definition.
#[derive(Clone, Copy)]
pub struct ExplainedQuery<'a> {
    pub plan: &'a uqa_sql::plan::QueryPlan,
    pub measurement: &'a QueryMeasurement,
}

#[derive(Clone, Copy)]
struct Running {
    started: Option<Instant>,
    downstream: Duration,
    first_output: Option<Duration>,
    rows: u64,
}

impl QueryMeasurement {
    pub(crate) fn new(timing: bool) -> Self {
        Self {
            clock: timing.then_some(Instant::now),
            running: Cell::new(None),
            completed: Cell::new(ExplainNodeMetrics::default()),
        }
    }

    pub(crate) fn metrics(&self) -> ExplainNodeMetrics {
        self.completed.get()
    }

    /// The execution owner starts and finishes one invocation; constructing or describing a destination does not count as execution.
    pub(crate) fn run<T>(
        &self,
        execute: impl FnOnce() -> Result<T, SQLError>,
    ) -> Result<T, SQLError> {
        if self.running.get().is_some() {
            return Err(SQLError::Internal(
                "query measurement already executing".into(),
            ));
        }
        self.running.set(Some(Running {
            started: self.now(),
            downstream: Duration::ZERO,
            first_output: None,
            rows: 0,
        }));
        let guard = Invocation(self);
        let result = execute();
        let completed = self.finish();
        drop(guard);
        combine(result, completed)
    }

    /// Count rows before handing them to the destination, then exclude only that handoff. Empty batches do not establish startup time.
    pub(crate) fn output<T>(
        &self,
        rows: usize,
        consume: impl FnOnce() -> Result<T, SQLError>,
    ) -> Result<T, SQLError> {
        let mut running = self.running.get().ok_or_else(inactive)?;
        running.rows = running
            .rows
            .checked_add(u64::try_from(rows).map_err(|_| overflow())?)
            .ok_or_else(overflow)?;
        if rows != 0 && running.first_output.is_none() {
            running.first_output = Some(self.elapsed(running)?);
        }
        self.running.set(Some(running));
        let started = self.now();
        let result = consume();
        let paused = self.pause(started);
        combine(result, paused)
    }

    fn now(&self) -> Option<Instant> {
        self.clock.map(|now| now())
    }

    fn elapsed(&self, running: Running) -> Result<Duration, SQLError> {
        match running.started {
            Some(started) => self
                .now()
                .expect("timed invocation")
                .checked_duration_since(started)
                .and_then(|elapsed| elapsed.checked_sub(running.downstream))
                .ok_or_else(overflow),
            None => Ok(Duration::ZERO),
        }
    }

    fn pause(&self, started: Option<Instant>) -> Result<(), SQLError> {
        if let Some(started) = started {
            let mut running = self.running.get().ok_or_else(inactive)?;
            running.downstream = running
                .downstream
                .checked_add(
                    self.now()
                        .expect("timed destination")
                        .checked_duration_since(started)
                        .ok_or_else(overflow)?,
                )
                .ok_or_else(overflow)?;
            self.running.set(Some(running));
        }
        Ok(())
    }

    fn finish(&self) -> Result<(), SQLError> {
        let running = self.running.get().ok_or_else(inactive)?;
        let elapsed = self.elapsed(running)?;
        let completed = self.completed.get();
        self.completed.set(ExplainNodeMetrics {
            rows: completed
                .rows
                .checked_add(running.rows)
                .ok_or_else(overflow)?,
            loops: completed.loops.checked_add(1).ok_or_else(overflow)?,
            startup: completed
                .startup
                .checked_add(running.first_output.unwrap_or(elapsed))
                .ok_or_else(overflow)?,
            total: completed.total.checked_add(elapsed).ok_or_else(overflow)?,
        });
        Ok(())
    }
}

struct Invocation<'a>(&'a QueryMeasurement);

impl Drop for Invocation<'_> {
    fn drop(&mut self) {
        self.0.running.set(None);
    }
}

fn inactive() -> SQLError {
    SQLError::Internal("query output measurement has no executing invocation".into())
}

fn overflow() -> SQLError {
    SQLError::Internal("EXPLAIN execution measurements overflowed".into())
}

fn combine<T>(
    result: Result<T, SQLError>,
    accounting: Result<(), SQLError>,
) -> Result<T, SQLError> {
    match (result, accounting) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(accounting)) => {
            Err(error.with_cleanup(accounting, "record EXPLAIN execution measurements"))
        }
    }
}
