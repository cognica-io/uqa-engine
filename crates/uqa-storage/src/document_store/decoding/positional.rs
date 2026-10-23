//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected document fields retain positional values without field-name maps.

use uqa_core::{
    memory::{Budgeted, BudgetedVec},
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
    let mut values = BudgetedVec::new(control.memory());
    let mut slots = BudgetedVec::new(control.memory());
    slots.reserve(fields.len())?;
    for _ in fields {
        slots.push(ABSENT)?;
    }
    let mut payload = control.memory().empty_reservation();
    let mut collect = |name: &str, value: Budgeted<Value>| {
        let mut selected = fields
            .iter()
            .enumerate()
            .filter(|(_, field)| **field == name);
        let Some((first, _)) = selected.next() else {
            return Ok(());
        };
        values.reserve(1)?;
        let slot = values.len();
        let (value, memory) = value.into_parts();
        payload.absorb(memory);
        values.push(value)?;
        slots[first] = slot;
        for (position, _) in selected {
            slots[position] = slot;
        }
        Ok(())
    };
    if !projected::visit(input, fields, control, &mut collect)? {
        legacy::visit_fields(input, control, &mut collect)?;
    }
    control.check()?;
    let (values, value_memory) = values.into_parts();
    let (slots, slot_memory) = slots.into_parts();
    payload.absorb(value_memory);
    payload.absorb(slot_memory);
    Ok(Budgeted::new(
        ProjectedDocumentFields { values, slots },
        payload,
    ))
}

#[cfg(test)]
mod tests;
