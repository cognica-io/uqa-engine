//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native JSONB parsing can retain its allocations under a caller's shared allowance.

use crate::{
    memory::{BudgetedVec, MemoryBudget, MemoryError, MemoryReservation},
    CancellationToken,
};

use super::{JsonNumber, JsonbKeyError};

pub(super) struct Workspace<'a> {
    memory: Option<MemoryReservation>,
    cancellation: Option<&'a CancellationToken>,
}

impl<'a> Workspace<'a> {
    pub(super) fn unbounded() -> Self {
        Self {
            memory: None,
            cancellation: None,
        }
    }

    pub(super) fn bounded(memory: &MemoryBudget, cancellation: &'a CancellationToken) -> Self {
        Self {
            memory: Some(memory.empty_reservation()),
            cancellation: Some(cancellation),
        }
    }

    pub(super) fn check(&self) -> Result<(), JsonbKeyError> {
        if let Some(cancellation) = self.cancellation {
            cancellation.check()?;
        }
        Ok(())
    }

    fn reserve(&self, bytes: usize) -> Result<Option<MemoryReservation>, JsonbKeyError> {
        self.check()?;
        self.memory
            .as_ref()
            .map(|memory| memory.budget().reserve(bytes))
            .transpose()
            .map_err(Into::into)
    }

    fn retain(&mut self, memory: MemoryReservation) {
        self.memory
            .as_mut()
            .expect("tracked parsing workspace")
            .absorb(memory);
    }

    fn retain_capacity(
        &mut self,
        memory: &mut Option<MemoryReservation>,
        capacity: usize,
    ) -> Result<(), JsonbKeyError> {
        if let Some(memory) = memory {
            memory.grow(capacity.saturating_sub(memory.bytes()))?;
            self.retain(memory.split(capacity));
        }
        Ok(())
    }

    pub(super) fn string(&mut self, encoded: &[u8]) -> Result<String, JsonbKeyError> {
        // Native string decoding can hold its geometrically grown escape scratch and the final UTF-8 string together. Decoded bytes cannot exceed the encoded input length; retain only the resulting string capacity after decoding.
        let bytes = encoded
            .len()
            .checked_mul(4)
            .and_then(|bytes| bytes.checked_add(16))
            .ok_or(MemoryError::SizeOverflow)?;
        let mut memory = self.reserve(bytes)?;
        let value =
            serde_json::from_slice::<String>(encoded).map_err(|_| JsonbKeyError::InvalidJson)?;
        self.retain_capacity(&mut memory, value.capacity())?;
        Ok(value)
    }

    pub(super) fn number(&mut self, text: &str) -> Result<JsonNumber, JsonbKeyError> {
        // The native digit collector grows geometrically from a minimum byte buffer. Its normalized digits remain owned by the parsed number.
        let bytes = text
            .len()
            .checked_mul(2)
            .and_then(|bytes| bytes.checked_add(8))
            .ok_or(MemoryError::SizeOverflow)?;
        let mut memory = self.reserve(bytes)?;
        let value = JsonNumber::parse(text).ok_or(JsonbKeyError::InvalidJson)?;
        self.retain_capacity(&mut memory, value.digits.capacity())?;
        Ok(value)
    }

    pub(super) fn buffer<T>(&self) -> ParseBuffer<T> {
        self.memory.as_ref().map_or_else(
            || ParseBuffer::Unbounded(Vec::new()),
            |memory| ParseBuffer::Bounded(BudgetedVec::new(memory.budget())),
        )
    }
}

pub(super) enum ParseBuffer<T> {
    Unbounded(Vec<T>),
    Bounded(BudgetedVec<T>),
}

impl<T> ParseBuffer<T> {
    pub(super) fn push(&mut self, value: T) -> Result<(), JsonbKeyError> {
        match self {
            Self::Unbounded(values) => values.push(value),
            Self::Bounded(values) => values.push(value)?,
        }
        Ok(())
    }

    pub(super) fn finish(self, workspace: &mut Workspace<'_>) -> Vec<T> {
        match self {
            Self::Unbounded(values) => values,
            Self::Bounded(values) => {
                let (values, memory) = values.into_parts();
                workspace.retain(memory);
                values
            }
        }
    }
}
