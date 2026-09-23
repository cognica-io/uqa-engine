//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native JSONB parsing can retain its allocations under a caller's shared allowance.

use crate::{
    json::{decode_json_string, decode_json_string_with_control, JsonReader},
    memory::{BudgetedVec, MemoryBudget, MemoryError, MemoryReservation, ProductionControl},
    CancellationToken,
};

use super::{JsonNumber, JsonbKeyError};

pub(super) struct Workspace<'a> {
    memory: Option<MemoryReservation>,
    cancellation: Option<&'a CancellationToken>,
    production: Option<ProductionControl<'a>>,
}

impl<'a> Workspace<'a> {
    pub(super) fn unbounded() -> Self {
        Self {
            memory: None,
            cancellation: None,
            production: None,
        }
    }

    pub(super) fn bounded(memory: &MemoryBudget, cancellation: &'a CancellationToken) -> Self {
        Self {
            memory: Some(memory.empty_reservation()),
            cancellation: Some(cancellation),
            production: None,
        }
    }

    pub(super) fn with_control(control: &ProductionControl<'a>) -> Self {
        Self {
            memory: control.empty_reservation(),
            cancellation: None,
            production: Some(*control),
        }
    }

    pub(super) fn check(&self) -> Result<(), JsonbKeyError> {
        if let Some(control) = self.production {
            control.check_cancellation()?;
        }
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

    pub(super) fn reader<'input>(&self, input: &'input str) -> JsonReader<'input, 'a> {
        if let Some(control) = self.production {
            return JsonReader::with_control(input, &control);
        }
        match (&self.memory, self.cancellation) {
            (Some(memory), Some(cancellation)) => {
                JsonReader::new(input, memory.budget(), cancellation)
            }
            _ => JsonReader::unbounded(input),
        }
    }

    pub(super) fn string(&mut self, encoded: &[u8]) -> Result<String, JsonbKeyError> {
        if let Some(control) = self.production {
            let (value, memory) = decode_json_string_with_control(encoded, &control)?.into_parts();
            if let Some(memory) = memory {
                self.retain(memory);
            }
            return Ok(value);
        }
        match (&self.memory, self.cancellation) {
            (Some(memory), Some(cancellation)) => {
                let (value, retained) =
                    decode_json_string(encoded, memory.budget(), cancellation)?.into_parts();
                self.retain(retained);
                Ok(value)
            }
            _ => serde_json::from_slice(encoded).map_err(|_| JsonbKeyError::InvalidJson),
        }
    }

    pub(super) fn number(&mut self, text: &str) -> Result<JsonNumber, JsonbKeyError> {
        // The native digit collector grows geometrically from a minimum byte buffer. Its normalized digits remain owned by the parsed number.
        let bytes = text
            .len()
            .checked_mul(2)
            .and_then(|bytes| bytes.checked_add(8))
            .ok_or(MemoryError::SizeOverflow)?;
        let mut memory = self.reserve(bytes)?;
        let value =
            JsonNumber::parse(text, &mut || self.check())?.ok_or(JsonbKeyError::InvalidJson)?;
        self.retain_capacity(&mut memory, value.digits.capacity())?;
        Ok(value)
    }

    pub(super) fn compare_text(
        &self,
        left: &str,
        right: &str,
    ) -> Result<std::cmp::Ordering, JsonbKeyError> {
        for (left, right) in left
            .as_bytes()
            .chunks(4096)
            .zip(right.as_bytes().chunks(4096))
        {
            self.check()?;
            let ordering = left.cmp(right);
            if !ordering.is_eq() {
                return Ok(ordering);
            }
        }
        self.check()?;
        Ok(left.len().cmp(&right.len()))
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
    pub(super) fn last_mut(&mut self) -> Option<&mut T> {
        match self {
            Self::Unbounded(values) => values.last_mut(),
            Self::Bounded(values) => values.last_mut(),
        }
    }

    pub(super) fn pop(&mut self) -> Option<T> {
        match self {
            Self::Unbounded(values) => values.pop(),
            Self::Bounded(values) => values.pop(),
        }
    }

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
