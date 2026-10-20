//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Captured AFTER events retain bounded encoded prefixes and spill without re-resolving triggers.

use serde::{de::DeserializeOwned, Serialize};
use std::marker::PhantomData;
use uqa_core::{CancellationToken, Value};
use uqa_sql::{error::Result, SQLError};

use super::{context::TriggerContext, AfterRowTriggerEvent};
use crate::physical::physical_exec_error;
use crate::spill::BufferedIndexedSpill;
use crate::{PhysicalRow, RowSchema};

pub(crate) mod values;

pub(super) struct EventBuffer<T> {
    rows: Option<BufferedIndexedSpill>,
    next: u64,
    event: PhantomData<T>,
}

impl<T> Default for EventBuffer<T> {
    fn default() -> Self {
        Self {
            rows: None,
            next: 0,
            event: PhantomData,
        }
    }
}

impl<T: Serialize + DeserializeOwned> EventBuffer<T> {
    pub(super) fn len(&self) -> u64 {
        self.rows.as_ref().map_or(0, BufferedIndexedSpill::len)
    }

    pub(super) fn position(&self) -> u64 {
        self.next
    }

    pub(super) fn push(
        &mut self,
        event: &T,
        work_mem: impl FnOnce() -> Result<usize>,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        cancellation.check()?;
        if self.rows.is_none() {
            self.rows = Some(BufferedIndexedSpill::new(
                RowSchema::new(vec!["event".into()]),
                work_mem()?.max(1),
            ));
        }
        let mut encoded = Vec::new();
        ciborium::ser::into_writer(event, &mut encoded)
            .map_err(|error| invalid(format!("cannot encode event: {error}")))?;
        self.rows
            .as_mut()
            .expect("initialized event buffer")
            .push(&PhysicalRow::from_values(vec![Value::Bytes(encoded)]))
            .map_err(physical_exec_error)
    }

    /// Reentrant trigger actions may append after this reader reaches its current end.
    pub(super) fn take_next(&mut self, cancellation: &CancellationToken) -> Result<Option<T>> {
        cancellation.check()?;
        if self.next == self.len() {
            return Ok(None);
        }
        let row = self
            .rows
            .as_mut()
            .expect("nonempty event buffer")
            .get(self.next)
            .map_err(physical_exec_error)?;
        let [Value::Bytes(encoded)] = <[Value; 1]>::try_from(row.into_physical_values())
            .map_err(|_| invalid("invalid event width"))?
        else {
            return Err(invalid("invalid event encoding"));
        };
        let mut input = std::io::Cursor::new(encoded.as_slice());
        let event = ciborium::de::from_reader(&mut input)
            .map_err(|error| invalid(format!("cannot decode event: {error}")))?;
        if input.position() != encoded.len() as u64 {
            return Err(invalid("trailing event data"));
        }
        self.next += 1;
        Ok(Some(event))
    }
}

/// Command-local AFTER events, before they enter the statement's reentrant queue.
#[derive(Default)]
pub struct AfterRowEvents {
    events: EventBuffer<AfterRowTriggerEvent>,
}

/// A single row may prepare several events before publication; complete commands use bounded storage.
pub trait AfterRowEventSink {
    fn push_event(
        &mut self,
        event: AfterRowTriggerEvent,
        context: &TriggerContext<'_>,
    ) -> Result<()>;
}

impl AfterRowEventSink for Vec<AfterRowTriggerEvent> {
    fn push_event(
        &mut self,
        event: AfterRowTriggerEvent,
        _context: &TriggerContext<'_>,
    ) -> Result<()> {
        self.push(event);
        Ok(())
    }
}

impl AfterRowEventSink for AfterRowEvents {
    fn push_event(
        &mut self,
        event: AfterRowTriggerEvent,
        context: &TriggerContext<'_>,
    ) -> Result<()> {
        self.push(
            event,
            || context.runtime.work_mem_bytes(),
            context.runtime.cancellation,
        )
    }
}

impl AfterRowEvents {
    pub(super) fn push(
        &mut self,
        event: AfterRowTriggerEvent,
        work_mem: impl FnOnce() -> Result<usize>,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        self.events.push(&event, work_mem, cancellation)
    }

    pub(super) fn take_next(
        &mut self,
        cancellation: &CancellationToken,
    ) -> Result<Option<AfterRowTriggerEvent>> {
        self.events.take_next(cancellation)
    }
}

fn invalid(message: impl std::fmt::Display) -> SQLError {
    SQLError::Internal(format!("AFTER trigger events: {message}"))
}

#[cfg(test)]
mod tests;
