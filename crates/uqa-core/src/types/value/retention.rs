//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reservations for the owned payload of immutable decoded values.

use super::Value;
use crate::{
    memory::{MemoryBudget, MemoryError, MemoryReservation, ProductionControl},
    CancellationToken, QueryCancelled,
};

mod stack;
use stack::Stack;

#[derive(Debug, thiserror::Error)]
pub enum ValueRetentionError {
    #[error(transparent)]
    Memory(#[from] MemoryError),
    #[error(transparent)]
    Cancelled(#[from] QueryCancelled),
    /// A recognized tagged carrier whose payload violates its format. Such
    /// input is a typed decoding failure, never an opaque document map.
    #[error("malformed {kind} value: {reason}")]
    Malformed { kind: &'static str, reason: String },
}

enum Children<'a> {
    Values(std::slice::Iter<'a, Value>),
    Record(crate::RecordIter<'a>),
    Map(std::collections::btree_map::Iter<'a, String, Value>),
}

impl<'a> Children<'a> {
    fn next(&mut self) -> Option<(&'a Value, usize)> {
        match self {
            Self::Values(values) => values.next().map(|value| (value, 0)),
            Self::Record(fields) => fields.next().map(|(name, value)| (value, name.capacity())),
            Self::Map(fields) => fields.next().map(|(name, value)| (value, name.capacity())),
        }
    }
}

fn buffer_bytes<T>(capacity: usize) -> Result<usize, MemoryError> {
    capacity
        .checked_mul(size_of::<T>())
        .ok_or(MemoryError::SizeOverflow)
}

impl Value {
    /// Reserve owned string/vector capacities, array headers, decimal payloads and live map entries, excluding this value's inline layout. B-tree node slack and allocator/reference-count bookkeeping are not exposed by these carriers and are outside this payload charge. The temporary traversal stack uses the same allowance and checks cancellation without recursive calls.
    pub fn reserve_retained_payload(
        &self,
        budget: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<MemoryReservation, ValueRetentionError> {
        self.reserve_retained_payload_with_check(budget, || cancellation.check())
    }

    /// Admit existing payload capacities while checking the caller's original and invoking scopes through one callback.
    pub fn reserve_retained_payload_with_check(
        &self,
        budget: &MemoryBudget,
        check: impl FnMut() -> Result<(), QueryCancelled>,
    ) -> Result<MemoryReservation, ValueRetentionError> {
        let mut memory = budget.empty_reservation();
        self.visit_retained_payload_with_check(budget, check, |bytes| memory.grow(bytes))?;
        Ok(memory)
    }

    /// Count the same owned payload as `reserve_retained_payload` without reserving that payload again. Only the temporary traversal stack uses the supplied allowance.
    pub fn retained_payload_bytes(
        &self,
        budget: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<usize, ValueRetentionError> {
        self.retained_payload_bytes_with_check(budget, || cancellation.check())
    }

    /// Count retained payload with caller-owned traversal scratch and cancellation checks. The count does not acquire a payload reservation or change the value's existing owner.
    pub fn retained_payload_bytes_with_check(
        &self,
        budget: &MemoryBudget,
        check: impl FnMut() -> Result<(), QueryCancelled>,
    ) -> Result<usize, ValueRetentionError> {
        let mut total = 0_usize;
        self.visit_retained_payload_with_check(budget, check, |bytes| {
            total = total.checked_add(bytes).ok_or(MemoryError::SizeOverflow)?;
            Ok(())
        })?;
        Ok(total)
    }

    /// Count the same native payload during owner construction. Ordinary builders use ordinary traversal scratch; controlled producers admit that scratch and preserve both cancellation scopes. No payload is copied or reserved by this observation.
    pub fn retained_payload_bytes_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<usize, ValueRetentionError> {
        let mut total = 0_usize;
        self.visit_retained_payload(
            control.budget(),
            || control.check_cancellation(),
            |bytes| {
                total = total.checked_add(bytes).ok_or(MemoryError::SizeOverflow)?;
                Ok(())
            },
        )?;
        Ok(total)
    }

    /// Observe the same retained payload one native field or traversal transition at a time. Zero-byte transitions are retained so returning through a deep container chain cannot consume an unbounded iterator step. Original and invoking cancellation scopes and admitted scratch remain active; completion or failure retires the traversal, and later calls return `None`.
    pub fn retained_payload_parts_with_control<'a>(
        &'a self,
        control: ProductionControl<'a>,
    ) -> impl Iterator<Item = Result<usize, ValueRetentionError>> + Send + 'a {
        let mut state = Some(Traversal::new(self, control.budget()));
        std::iter::from_fn(move || {
            let mut traversal = state.take()?;
            let result = control
                .check_cancellation()
                .map_err(ValueRetentionError::from)
                .and_then(|()| traversal.step().map_err(ValueRetentionError::from));
            match result {
                Ok(Some(bytes)) => {
                    state = Some(traversal);
                    Some(Ok(bytes))
                }
                Ok(None) => None,
                Err(error) => Some(Err(error)),
            }
        })
    }

    pub(crate) fn visit_retained_payload_with_check(
        &self,
        budget: &MemoryBudget,
        check: impl FnMut() -> Result<(), QueryCancelled>,
        visit: impl FnMut(usize) -> Result<(), MemoryError>,
    ) -> Result<(), ValueRetentionError> {
        self.visit_retained_payload(Some(budget), check, visit)
    }

    fn visit_retained_payload(
        &self,
        budget: Option<&MemoryBudget>,
        mut check: impl FnMut() -> Result<(), QueryCancelled>,
        mut visit: impl FnMut(usize) -> Result<(), MemoryError>,
    ) -> Result<(), ValueRetentionError> {
        let mut traversal = Traversal::new(self, budget);
        loop {
            check()?;
            match traversal.step()? {
                Some(bytes) => visit(bytes)?,
                None => return Ok(()),
            }
        }
    }
}

enum Phase<'a> {
    Name(&'a Value, usize),
    Payload(&'a Value),
    Push(Option<Children<'a>>),
    Next,
}

struct Traversal<'a> {
    stack: Stack<'a>,
    phase: Phase<'a>,
}

impl<'a> Traversal<'a> {
    fn new(value: &'a Value, budget: Option<&MemoryBudget>) -> Self {
        Self {
            stack: Stack::new(budget),
            phase: Phase::Name(value, 0),
        }
    }

    fn step(&mut self) -> Result<Option<usize>, MemoryError> {
        match std::mem::replace(&mut self.phase, Phase::Next) {
            Phase::Name(value, bytes) => {
                self.phase = Phase::Payload(value);
                return Ok(Some(bytes));
            }
            Phase::Payload(value) => {
                let (bytes, children) = payload(value)?;
                self.phase = Phase::Push(children);
                return Ok(Some(bytes));
            }
            Phase::Push(children) => {
                if let Some(children) = children {
                    self.stack.push(children)?;
                }
            }
            Phase::Next => match self.stack.last_mut() {
                Some(children) => {
                    if let Some((value, bytes)) = children.next() {
                        self.phase = Phase::Name(value, bytes);
                    } else {
                        self.stack.pop();
                    }
                }
                None => return Ok(None),
            },
        }
        Ok(Some(0))
    }
}

fn payload(value: &Value) -> Result<(usize, Option<Children<'_>>), MemoryError> {
    Ok(match value {
        Value::Null
        | Value::Void
        | Value::Bool(_)
        | Value::Int(_)
        | Value::Float(_)
        | Value::Temporal(_) => (0, None),
        Value::Str(text) | Value::FixedChar(text) | Value::Json(text) | Value::JsonB(text) => {
            (text.capacity(), None)
        }
        Value::Bytes(bytes) => (bytes.capacity(), None),
        Value::BitString(value) => (value.retained_bytes(), None),
        Value::Datum(value) => (value.retained_bytes(), None),
        Value::Enum(value) => (value.retained_bytes(), None),
        Value::Decimal(decimal) => (decimal.retained_bytes(), None),
        Value::Array(array) => (
            array.retained_buffer_bytes()?,
            (!array.elements().is_empty()).then(|| Children::Values(array.elements().iter())),
        ),
        Value::LegacyVector(vector) => (vector.as_array().retained_buffer_bytes()?, None),
        Value::List(values) => (
            buffer_bytes::<Value>(values.capacity())?,
            (!values.is_empty()).then(|| Children::Values(values.iter())),
        ),
        Value::Row(row) => (
            row.retained_buffer_bytes()?,
            (!row.is_empty()).then(|| Children::Values(row.iter())),
        ),
        Value::Record(fields) => (
            fields.retained_buffer_bytes()?,
            (!fields.is_empty()).then(|| Children::Record(fields.iter())),
        ),
        Value::Map(fields) => (
            buffer_bytes::<(String, Value)>(fields.len())?,
            (!fields.is_empty()).then(|| Children::Map(fields.iter())),
        ),
    })
}

#[cfg(test)]
mod tests;
