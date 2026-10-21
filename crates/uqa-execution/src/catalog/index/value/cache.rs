//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded evaluated-key retention preserves the existing SQL eligibility policy.

use super::{comparison, ColumnValueIndex};
use std::borrow::Cow;
use uqa_core::{DocId, Predicate, Value};
use uqa_sql::SQLError;
use uqa_storage::{
    btree_index::retained::RetainedValueIndex, read_control::StorageReadControl,
    StorageBackendError,
};

mod builder;
mod probes;
mod projection;
mod publication;
pub use builder::ValueIndexBuilder;
pub use projection::visit_stored_fields;
pub use publication::apply_cached_values;

#[derive(Clone)]
pub struct ValueIndexCache {
    // With retained storage this holds eligibility flags and no row population.
    policy: ColumnValueIndex,
    retained: Option<RetainedValueIndex>,
    field: String,
    limit: usize,
    resident_bytes: usize,
}

fn storage(error: StorageBackendError) -> SQLError {
    crate::storage_errors::storage_error("retained value index", &error)
}

fn value_bytes(value: &Value, control: &StorageReadControl) -> Result<usize, SQLError> {
    let payload = value
        .retained_payload_bytes(control.memory(), control.cancellation())
        .map_err(|error| storage(StorageBackendError::backend("retained key", error)))?;
    // Both the identity map and the search tree may own the value. The allowance also covers their nodes, NULL identities and key headers.
    payload
        .checked_add(std::mem::size_of::<Value>())
        .and_then(|bytes| bytes.checked_mul(2))
        .and_then(|bytes| bytes.checked_add(128))
        .ok_or_else(|| storage(uqa_core::memory::MemoryError::SizeOverflow.into()))
}

fn observe(policy: &mut ColumnValueIndex, value: &Value) {
    policy.has_temporal |= super::value_is_temporal(value);
    policy.has_row_values |= super::value_is_row(value);
    policy.comparison.include(value);
}

impl ValueIndexCache {
    pub fn is_carried(&self) -> bool {
        self.policy.is_carried()
    }

    pub fn is_spilled(&self) -> bool {
        self.retained.is_some()
    }

    pub fn stored_value(
        &self,
        id: DocId,
        control: &StorageReadControl,
    ) -> Result<Option<Cow<'_, Value>>, SQLError> {
        match &self.retained {
            Some(values) => values
                .get(id, control)
                .map(|value| value.map(Cow::Owned))
                .map_err(storage),
            None => Ok(self.policy.stored_value(id).map(Cow::Borrowed)),
        }
    }

    pub fn contains(&self, id: DocId, control: &StorageReadControl) -> Result<bool, SQLError> {
        self.retained.as_ref().map_or_else(
            || Ok(self.policy.contains(id)),
            |values| values.contains(id, control).map_err(storage),
        )
    }

    /// Stored expression keys remain the old source of truth when removing a row. A failed migration or replacement publishes neither a partial tree nor new eligibility flags.
    pub fn replace(
        &mut self,
        id: DocId,
        value: Option<&Value>,
        control: &StorageReadControl,
    ) -> Result<(), SQLError> {
        let prepared = self.prepare_replace(id, value, control)?;
        self.apply_prepared(prepared);
        Ok(())
    }

    fn prepare_replace<'a>(
        &self,
        id: DocId,
        value: Option<&'a Value>,
        control: &StorageReadControl,
    ) -> Result<publication::PreparedChange<'a>, SQLError> {
        control.check().map_err(storage)?;
        if self.retained.is_some() {
            let mut replacement = self.clone();
            let values = replacement.retained.as_mut().expect("retained index");
            values.replace(id, value, control).map_err(storage)?;
            if let Some(value) = value {
                observe(&mut replacement.policy, value);
            }
            return Ok(publication::PreparedChange::Replacement(replacement));
        }
        let previous = self
            .policy
            .stored_value(id)
            .map(|value| value_bytes(value, control))
            .transpose()?
            .unwrap_or(0);
        let new_bytes = value
            .map(|value| value_bytes(value, control))
            .transpose()?
            .unwrap_or(0);
        let bytes = self
            .resident_bytes
            .checked_sub(previous)
            .and_then(|bytes| bytes.checked_add(new_bytes))
            .ok_or_else(|| storage(uqa_core::memory::MemoryError::SizeOverflow.into()))?;
        if bytes > self.limit {
            let mut builder = ValueIndexBuilder::new(&self.field, self.is_carried(), self.limit)?;
            for (&old_id, old) in &self.policy.values {
                if old_id != id {
                    builder.push(old_id, old.clone(), control)?;
                }
            }
            if let Some(value) = value {
                builder.push(id, value.clone(), control)?;
            }
            let mut replacement = builder.finish(control)?;
            // Removal never made the old accelerator less conservative; changing its backing must not do so either.
            replacement.policy.has_temporal |= self.policy.has_temporal;
            replacement.policy.has_row_values |= self.policy.has_row_values;
            if self.policy.comparison.can_fail() {
                replacement.policy.comparison = comparison::StoredComparison::Fallible;
            } else if self.policy.comparison.field_needs_evaluation()
                && !replacement.policy.comparison.can_fail()
            {
                replacement.policy.comparison = comparison::StoredComparison::TypedField;
            }
            return Ok(publication::PreparedChange::Replacement(replacement));
        }
        Ok(publication::PreparedChange::Resident { id, value, bytes })
    }

    #[must_use]
    pub fn with_use(mut self, field: &str, carried: bool) -> Self {
        self.set_use(field, carried);
        self
    }

    pub fn set_use(&mut self, field: &str, carried: bool) {
        if self.retained.is_some() {
            self.policy.index = (!carried).then(|| uqa_storage::BTreeIndex::new(field));
        } else {
            let previous = std::mem::replace(
                &mut self.policy,
                ColumnValueIndex::build_carried(std::iter::empty()),
            );
            self.policy = previous.with_use(field, carried);
        }
        self.field = field.to_string();
    }

    pub fn clear(&mut self) {
        self.policy.clear();
        if let Some(values) = &mut self.retained {
            values.clear();
        }
        self.resident_bytes = 0;
    }

    pub fn supports(&self, predicate: &Predicate) -> bool {
        self.policy.supports(predicate)
    }
}

#[cfg(test)]
mod tests;
