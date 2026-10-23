//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected document fields retain positional values without field-name maps.

use uqa_core::{
    memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryError, MemoryReservation},
    Value,
};

use super::{legacy, projected, text, StorageBackendResult, StorageReadControl};

const ABSENT: usize = usize::MAX;

/// Decoded values and requested-position mapping. Repeated requested names
/// share one value; absent fields differ from explicitly stored SQL NULL.
#[derive(Debug)]
pub struct ProjectedDocumentFields {
    values: Vec<Value>,
    slots: Vec<usize>,
}

impl ProjectedDocumentFields {
    /// A missing field or position returns None; a stored NULL returns Some.
    pub fn get(&self, position: usize) -> Option<&Value> {
        self.values.get(*self.slots.get(position)?)
    }

    /// Mutate a present value through one position; every alias observes it.
    /// The caller retains the producer allowance for any replacement payload.
    pub fn get_mut(&mut self, position: usize) -> Option<&mut Value> {
        self.values.get_mut(*self.slots.get(position)?)
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Distinct retained values, which may be fewer than requested positions.
    pub fn values(&self) -> &[Value] {
        &self.values
    }
}

/// Decode requested positions with the ordinary legacy validation, normalization
/// and tagged-value rules. The returned lease covers the positional buffers and
/// every retained payload. Field names are never copied into the result.
pub fn decode_legacy_document_slots_budgeted(
    bytes: &[u8],
    fields: &[&str],
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<ProjectedDocumentFields>> {
    control.check()?;
    decode_legacy_document_text_slots_budgeted(text(bytes)?, fields, control)
}

pub fn decode_legacy_document_text_slots_budgeted(
    input: &str,
    fields: &[&str],
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<ProjectedDocumentFields>> {
    control.check()?;
    let mut builder = ProjectedFieldsBuilder::new(fields, control.memory())?;
    visit_legacy_fields(input, fields, control, &mut |name, value| {
        builder.push(name, value).map_err(Into::into)
    })?;
    control.check()?;
    Ok(builder.finish())
}

pub(crate) fn visit_legacy_fields(
    input: &str,
    fields: &[&str],
    control: &StorageReadControl,
    visitor: &mut dyn FnMut(&str, Budgeted<Value>) -> StorageBackendResult<()>,
) -> StorageBackendResult<()> {
    if !projected::visit(input, fields, control, visitor)? {
        legacy::visit_fields(input, control, visitor)?;
    }
    control.check()
}

/// Staging keeps per-value reservations so a duplicate stored name replaces
/// its earlier payload without accumulating values or retaining unused keys.
pub(crate) struct ProjectedFieldsBuilder<'a> {
    fields: &'a [&'a str],
    values: BudgetedVec<Value>,
    slots: BudgetedVec<usize>,
    payload_bytes: BudgetedVec<usize>,
    payload: MemoryReservation,
}

impl<'a> ProjectedFieldsBuilder<'a> {
    pub(crate) fn new(fields: &'a [&'a str], memory: &MemoryBudget) -> Result<Self, MemoryError> {
        let mut slots = BudgetedVec::new(memory);
        slots.reserve(fields.len())?;
        for _ in fields {
            slots.push(ABSENT)?;
        }
        Ok(Self {
            fields,
            values: BudgetedVec::new(memory),
            slots,
            payload_bytes: BudgetedVec::new(memory),
            payload: memory.empty_reservation(),
        })
    }

    pub(crate) fn push(&mut self, name: &str, value: Budgeted<Value>) -> Result<(), MemoryError> {
        let mut selected = self
            .fields
            .iter()
            .enumerate()
            .filter(|(_, field)| **field == name);
        let Some((first, _)) = selected.next() else {
            return Ok(());
        };
        let previous = self.slots[first];
        if previous != ABSENT {
            let bytes = value.reserved_bytes();
            let (value, memory) = value.into_parts();
            self.values[previous] = value;
            drop(self.payload.split(self.payload_bytes[previous]));
            self.payload_bytes[previous] = bytes;
            self.payload.absorb(memory);
            return Ok(());
        }
        self.values.reserve(1)?;
        self.payload_bytes.reserve(1)?;
        let slot = self.values.len();
        let bytes = value.reserved_bytes();
        let (value, memory) = value.into_parts();
        self.payload.absorb(memory);
        self.values.push(value)?;
        self.payload_bytes.push(bytes)?;
        self.slots[first] = slot;
        for (position, _) in selected {
            self.slots[position] = slot;
        }
        Ok(())
    }

    pub(crate) fn finish(mut self) -> Budgeted<ProjectedDocumentFields> {
        let (values, value_memory) = self.values.into_parts();
        let (slots, slot_memory) = self.slots.into_parts();
        self.payload.absorb(value_memory);
        self.payload.absorb(slot_memory);
        Budgeted::new(ProjectedDocumentFields { values, slots }, self.payload)
    }
}

#[cfg(test)]
mod tests;
