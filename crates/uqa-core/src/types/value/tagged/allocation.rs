//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Tagged conversion retains the input lease while old and replacement payloads overlap.

use crate::{
    memory::{MemoryError, MemoryReservation},
    ArrayValue, CancellationToken, DecimalValue, Value, ValueRetentionError,
};

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

    pub(super) fn bounded(memory: MemoryReservation, cancellation: &'a CancellationToken) -> Self {
        Self {
            memory: Some(memory),
            cancellation: Some(cancellation),
        }
    }

    pub(super) fn check(&self) -> Result<(), ValueRetentionError> {
        if let Some(cancellation) = self.cancellation {
            cancellation.check()?;
        }
        Ok(())
    }

    pub(super) fn reserve(&mut self, bytes: usize) -> Result<(), ValueRetentionError> {
        self.check()?;
        if let Some(memory) = &mut self.memory {
            memory.grow(bytes)?;
        }
        Ok(())
    }

    pub(super) fn vector<T>(&mut self, capacity: usize) -> Result<Vec<T>, ValueRetentionError> {
        let bytes = capacity
            .checked_mul(size_of::<T>())
            .ok_or(MemoryError::SizeOverflow)?;
        self.reserve(bytes)?;
        let mut values = Vec::new();
        values
            .try_reserve_exact(capacity)
            .map_err(MemoryError::from)?;
        let actual = values
            .capacity()
            .checked_mul(size_of::<T>())
            .ok_or(MemoryError::SizeOverflow)?;
        self.reserve(actual.saturating_sub(bytes))?;
        Ok(values)
    }

    pub(super) fn decimal(
        &mut self,
        text: &str,
    ) -> Result<Option<DecimalValue>, ValueRetentionError> {
        match (&self.memory, self.cancellation) {
            (Some(memory), Some(cancellation)) => {
                let Some(value) =
                    DecimalValue::parse_budgeted(text, memory.budget(), cancellation)?
                else {
                    return Ok(None);
                };
                let (value, memory) = value.into_parts();
                self.absorb(memory);
                Ok(Some(value))
            }
            _ => Ok(DecimalValue::parse(text)),
        }
    }

    pub(super) fn shape(
        &mut self,
        elements: &[Value],
        bound_count: usize,
    ) -> Result<Option<Vec<usize>>, ValueRetentionError> {
        match (&self.memory, self.cancellation) {
            (Some(memory), Some(cancellation)) => {
                let Some(shape) = ArrayValue::decoded_shape_budgeted(
                    elements,
                    bound_count,
                    memory.budget(),
                    cancellation,
                )?
                else {
                    return Ok(None);
                };
                let (dimensions, memory) = shape.into_parts();
                self.absorb(memory);
                Ok(Some(dimensions))
            }
            _ => Ok(ArrayValue::decoded_shape(elements, bound_count)),
        }
    }

    fn absorb(&mut self, memory: MemoryReservation) {
        self.memory
            .as_mut()
            .expect("controlled tagged conversion")
            .absorb(memory);
    }

    pub(super) fn retained(
        &mut self,
        value: &Value,
    ) -> Result<MemoryReservation, ValueRetentionError> {
        let memory = self.memory.as_mut().expect("controlled tagged conversion");
        let cancellation = self.cancellation.expect("controlled tagged conversion");
        let bytes = value.retained_payload_bytes(memory.budget(), cancellation)?;
        assert!(bytes <= memory.bytes(), "tagged conversion payload lease");
        Ok(memory.split(bytes))
    }
}
