//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepare every field before publishing a row's cached keys.

use super::ValueIndexCache;
use std::collections::BTreeMap;
use uqa_core::{DocId, Value};
use uqa_sql::SQLError;
use uqa_storage::{read_control::StorageReadControl, ValueIndexKey};

pub(super) enum PreparedChange<'a> {
    Resident {
        id: DocId,
        value: Option<&'a Value>,
        bytes: usize,
    },
    Replacement(ValueIndexCache),
}

impl ValueIndexCache {
    pub(super) fn apply_prepared(&mut self, change: PreparedChange<'_>) {
        match change {
            PreparedChange::Resident { id, value, bytes } => {
                self.policy.remove(id, &Value::Null);
                if let Some(value) = value {
                    self.policy.insert(id, value);
                }
                self.resident_bytes = bytes;
            }
            PreparedChange::Replacement(replacement) => *self = replacement,
        }
    }
}

/// A failed field read, migration or replacement leaves every cache unchanged. Stored old keys remain authoritative even if the defining expression has since changed.
pub fn apply_cached_values(
    indexes: &mut BTreeMap<ValueIndexKey, ValueIndexCache>,
    id: DocId,
    new: Option<&BTreeMap<ValueIndexKey, Value>>,
    control: &StorageReadControl,
) -> Result<(), SQLError> {
    let prepared = indexes
        .iter()
        .map(|(field, index)| {
            let value = new.map(|values| values.get(field).unwrap_or(&Value::Null));
            Ok((field.clone(), index.prepare_replace(id, value, control)?))
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    for (field, change) in prepared {
        indexes
            .get_mut(&field)
            .expect("prepared cache remains selected")
            .apply_prepared(change);
    }
    Ok(())
}
