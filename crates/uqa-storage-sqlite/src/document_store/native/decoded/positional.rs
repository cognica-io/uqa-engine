//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native projection decoding and selected compound hydration stay positional.

use rusqlite::types::ValueRef;
use uqa_core::memory::{Budgeted, BudgetedVec};
use uqa_storage::document_store::decoding::{
    decode_legacy_document_text_slots_budgeted, ProjectedDocumentFields,
};

use super::{controlled, DocId, NativeDocumentRead, SQLiteError, SQLiteResult};

pub(super) type Projected = Budgeted<ProjectedDocumentFields>;

pub(super) fn has_external_values(row: &ProjectedDocumentFields) -> bool {
    row.values()
        .iter()
        .any(|value| controlled::marker(value).is_some())
}

#[cfg(test)]
mod tests;

impl NativeDocumentRead<'_> {
    pub(super) fn decode_positional_body(
        &self,
        id: DocId,
        row: &[ValueRef<'_>],
        fields: &[&str],
    ) -> SQLiteResult<Projected> {
        let body = row[2]
            .as_str()
            .map_err(|_| SQLiteError::StorageBackend("native document body must be text".into()))?;
        let decoded = decode_legacy_document_text_slots_budgeted(body, fields, self.control)?;
        // Projection does not expose tuple metadata, but malformed metadata
        // retains the same validation point as an ordinary decoded document.
        super::super::read::metadata(row[3], self.table, id)?;
        Ok(decoded)
    }

    pub(super) fn hydrate_positional(
        &self,
        id: DocId,
        row: Projected,
        fields: &[&str],
    ) -> SQLiteResult<Projected> {
        self.snapshot.control.check()?;
        self.control.check()?;
        if !has_external_values(&row) {
            return Ok(row);
        }
        let (mut row, mut memory) = row.into_parts();
        // Named documents hydrated in field-name order. Keep that diagnostic
        // order even when the requested positions use a different order.
        let mut ordered = BudgetedVec::new(self.control.memory());
        for (position, field) in fields.iter().enumerate() {
            ordered.push((position, *field))?;
        }
        ordered.sort_unstable_by(|left, right| left.1.cmp(right.1));
        for (position, field) in ordered.iter().copied() {
            self.snapshot.control.check()?;
            self.control.check()?;
            let Some(marker) = row.get(position).and_then(controlled::marker) else {
                continue;
            };
            let value = self.hydrate_retained(id, field, marker)?;
            let (value, retained) = value.into_parts();
            *row.get_mut(position)
                .expect("selected marker remains present") = value;
            memory.absorb(retained);
        }
        self.snapshot.control.check()?;
        self.control.check()?;
        Ok(Budgeted::new(row, memory))
    }
}
