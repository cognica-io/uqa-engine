//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::SQLError;
use uqa_core::memory::{MemoryReservation, Produced, ProductionControl};

/// The immutable enclosing object owns these buffers. Ordinary copy-on-write creates new ordinary buffers; sharing the enclosing Arc retains this one lease.
#[derive(Debug, Default)]
pub(in crate::schema) struct SchemaMemory {
    _reservation: Option<MemoryReservation>,
}

impl SchemaMemory {
    pub(in crate::schema) fn new(memory: Option<MemoryReservation>) -> Self {
        Self {
            _reservation: memory,
        }
    }
}

impl Clone for SchemaMemory {
    fn clone(&self) -> Self {
        Self::default()
    }
}
impl PartialEq for SchemaMemory {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}
impl Eq for SchemaMemory {}

pub(in crate::schema) fn boxed_text(
    text: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<Box<str>>, SQLError> {
    let memory = control.reserve(text.len())?;
    Ok(control.finish(Box::from(text), memory)?)
}

/// Vec-to-box conversion may replace a spare-capacity buffer. Admit that exact destination while the old capacity and every element remain owned, then release only the old buffer's lease after conversion.
pub(super) fn boxed_slice<T>(
    value: Produced<Vec<T>>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Box<[T]>>, SQLError> {
    if value.len() == value.capacity() || size_of::<T>() == 0 {
        let (value, memory) = value.into_parts();
        return Ok(control.finish(value.into_boxed_slice(), memory)?);
    }
    let replacement = control.reserve(value.len() * size_of::<T>())?;
    let (value, mut memory) = value.into_parts();
    let old_buffer = memory
        .as_mut()
        .map(|memory| memory.split(value.capacity() * size_of::<T>()));
    let value = value.into_boxed_slice();
    drop(old_buffer);
    Ok(control.finish(value, control.combine(memory, replacement))?)
}
