//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Tagged conversion retains the input lease while old and replacement payloads overlap.

use crate::{
    memory::{MemoryError, MemoryReservation, ProductionControl, ProductionString},
    types::work::{checkpoint, Work},
    ArrayValue, DecimalValue, Value, ValueRetentionError,
};

#[cfg(test)]
thread_local! {
    pub(super) static AFTER_VECTOR_ALLOCATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

pub(super) struct Workspace<'a> {
    memory: Option<MemoryReservation>,
    control: ProductionControl<'a>,
}

impl<'a> Workspace<'a> {
    pub(super) fn new(memory: Option<MemoryReservation>, control: ProductionControl<'a>) -> Self {
        Self { memory, control }
    }

    pub(super) async fn checkpoint<W: Work>(&self, work: &mut W) -> Result<(), W::Error> {
        checkpoint(work, &mut || self.control.check()).await
    }

    pub(super) fn reserve(&mut self, bytes: usize) -> Result<(), ValueRetentionError> {
        self.control.check()?;
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
        #[cfg(test)]
        AFTER_VECTOR_ALLOCATION.with(|hook| {
            if let Some(callback) = hook.borrow_mut().take() {
                callback();
            }
        });
        Ok(values)
    }

    pub(super) async fn decimal<W: Work>(
        &mut self,
        text: &str,
        work: &mut W,
    ) -> Result<Option<DecimalValue>, W::Error> {
        let Some(value) = DecimalValue::parse_with_work(text, &self.control, work).await? else {
            return Ok(None);
        };
        let (value, memory) = value.into_parts();
        self.absorb(memory);
        Ok(Some(value))
    }

    pub(super) async fn shape<W: Work>(
        &mut self,
        elements: &[Value],
        bound_count: usize,
        work: &mut W,
    ) -> Result<Option<Vec<usize>>, W::Error> {
        let Some(shape) =
            ArrayValue::decoded_shape_with_work(elements, bound_count, &self.control, work).await?
        else {
            return Ok(None);
        };
        let (dimensions, memory) = shape.into_parts();
        self.absorb(memory);
        Ok(Some(dimensions))
    }

    pub(super) async fn normalize<W: Work>(
        &mut self,
        elements: &mut [Value],
        rank: usize,
        work: &mut W,
    ) -> Result<(), W::Error> {
        ArrayValue::normalize_decoded_with_work(
            elements,
            rank,
            &mut self.memory,
            &self.control,
            work,
        )
        .await
    }

    pub(super) async fn text<W: Work>(
        &mut self,
        text: &str,
        work: &mut W,
    ) -> Result<String, W::Error> {
        let mut output = ProductionString::new(self.control);
        output.reserve(text.len())?;
        let mut begin = 0;
        while begin < text.len() {
            self.checkpoint(work).await?;
            let mut end = (begin + 4096).min(text.len());
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            output.push_str(&text[begin..end])?;
            begin = end;
        }
        let (text, memory) = output.finish()?.into_parts();
        self.absorb(memory);
        Ok(text)
    }

    fn absorb(&mut self, memory: Option<MemoryReservation>) {
        self.memory = self.control.combine(self.memory.take(), memory);
    }

    pub(super) async fn retained<W: Work>(
        &mut self,
        value: &Value,
        work: &mut W,
    ) -> Result<Option<MemoryReservation>, W::Error> {
        if self.memory.is_none() {
            return Ok(None);
        }
        let mut bytes = 0usize;
        for part in value.retained_payload_parts_with_control(self.control) {
            self.checkpoint(work).await?;
            bytes = bytes
                .checked_add(part?)
                .ok_or(MemoryError::SizeOverflow)
                .map_err(ValueRetentionError::from)?;
        }
        self.checkpoint(work).await?;
        let memory = self.memory.as_mut().expect("controlled tagged conversion");
        assert!(bytes <= memory.bytes(), "tagged conversion payload lease");
        Ok(Some(memory.split(bytes)))
    }
}
