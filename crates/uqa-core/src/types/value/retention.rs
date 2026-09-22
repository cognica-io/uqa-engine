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
        let mut memory = budget.empty_reservation();
        let mut stack = BudgetedVec::new(budget);
        let mut current = Some((self, 0));
        loop {
            cancellation.check()?;
            if let Some((value, name_bytes)) = current.take() {
                memory.grow(name_bytes)?;
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
                memory.grow(bytes)?;
                if let Some(children) = children {
                    stack.push(children)?;
                }
            }
            while let Some(children) = stack.last_mut() {
                cancellation.check()?;
                current = children.next();
                if current.is_some() {
                    break;
                }
                stack.pop();
            }
            if current.is_none() {
                return Ok(memory);
            }
        }
    }
}

#[cfg(test)]
mod tests;
