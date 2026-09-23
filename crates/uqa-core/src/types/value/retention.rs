//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reservations for the owned payload of immutable decoded values.

use super::Value;
use crate::{
    memory::{BudgetedVec, MemoryBudget, MemoryError, MemoryReservation},
    CancellationToken, QueryCancelled,
};

#[derive(Debug, thiserror::Error)]
pub enum ValueRetentionError {
    #[error(transparent)]
    Memory(#[from] MemoryError),
    #[error(transparent)]
    Cancelled(#[from] QueryCancelled),
}

enum Children<'a> {
    Values(std::slice::Iter<'a, Value>),
    Record(std::slice::Iter<'a, (String, Value)>),
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
        let mut total = 0_usize;
        self.visit_retained_payload_with_check(
            budget,
            || cancellation.check(),
            |bytes| {
                total = total.checked_add(bytes).ok_or(MemoryError::SizeOverflow)?;
                Ok(())
            },
        )?;
        Ok(total)
    }

    pub(crate) fn visit_retained_payload_with_check(
        &self,
        budget: &MemoryBudget,
        mut check: impl FnMut() -> Result<(), QueryCancelled>,
        mut visit: impl FnMut(usize) -> Result<(), MemoryError>,
    ) -> Result<(), ValueRetentionError> {
        let mut stack = BudgetedVec::new(budget);
        let mut current = Some((self, 0));
        loop {
            check()?;
            if let Some((value, name_bytes)) = current.take() {
                visit(name_bytes)?;
                let (bytes, children) = match value {
                    Self::Null
                    | Self::Void
                    | Self::Bool(_)
                    | Self::Int(_)
                    | Self::Float(_)
                    | Self::Temporal(_) => (0, None),
                    Self::Str(text)
                    | Self::FixedChar(text)
                    | Self::Json(text)
                    | Self::JsonB(text) => (text.capacity(), None),
                    Self::Bytes(bytes) => (bytes.capacity(), None),
                    Self::Decimal(decimal) => (decimal.retained_bytes(), None),
                    Self::Array(array) => (
                        array.retained_buffer_bytes()?,
                        (!array.elements().is_empty())
                            .then(|| Children::Values(array.elements().iter())),
                    ),
                    Self::List(values) | Self::Row(values) => (
                        buffer_bytes::<Value>(values.capacity())?,
                        (!values.is_empty()).then(|| Children::Values(values.iter())),
                    ),
                    Self::Record(fields) => (
                        buffer_bytes::<(String, Value)>(fields.capacity())?,
                        (!fields.is_empty()).then(|| Children::Record(fields.iter())),
                    ),
                    Self::Map(fields) => (
                        buffer_bytes::<(String, Value)>(fields.len())?,
                        (!fields.is_empty()).then(|| Children::Map(fields.iter())),
                    ),
                };
                visit(bytes)?;
                if let Some(children) = children {
                    stack.push(children)?;
                }
            }
            while let Some(children) = stack.last_mut() {
                check()?;
                current = children.next();
                if current.is_some() {
                    break;
                }
                stack.pop();
            }
            if current.is_none() {
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests;
