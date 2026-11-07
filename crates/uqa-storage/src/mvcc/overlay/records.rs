//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native record publication maintains its complete independently retained payload footprint.

use super::{Change, RecordKey};
use std::ops::Deref;
use uqa_core::memory::{BudgetedSharedMap, BudgetedSharedMapIter, MemoryBudget, MemoryError};

#[derive(Clone)]
pub(super) struct Records {
    values: BudgetedSharedMap<RecordKey, Change>,
    payload_bytes: usize,
}

impl Records {
    pub(super) fn new(memory: &MemoryBudget) -> Self {
        Self {
            values: BudgetedSharedMap::new(memory),
            payload_bytes: 0,
        }
    }

    pub(super) fn retained_bytes(&self) -> Result<usize, MemoryError> {
        self.values
            .snapshot()
            .structural_bytes()?
            .checked_add(self.payload_bytes)
            .ok_or(MemoryError::SizeOverflow)
    }

    pub(super) fn try_insert(&mut self, key: RecordKey, value: Change) -> Result<(), MemoryError> {
        let previous = self
            .values
            .get(&key)
            .map(|value| value.write.retained_bytes())
            .transpose()?
            .unwrap_or(0);
        let added = value.write.retained_bytes()?;
        let bytes = self
            .payload_bytes
            .checked_sub(previous)
            .and_then(|bytes| bytes.checked_add(added))
            .ok_or(MemoryError::SizeOverflow)?;
        self.values.try_insert(key, value)?;
        self.payload_bytes = bytes;
        Ok(())
    }

    pub(super) fn with_insert(&self, key: RecordKey, value: Change) -> Result<Self, MemoryError> {
        let mut next = self.clone();
        next.try_insert(key, value)?;
        Ok(next)
    }
}

impl Deref for Records {
    type Target = BudgetedSharedMap<RecordKey, Change>;
    fn deref(&self) -> &Self::Target {
        &self.values
    }
}

impl<'a> IntoIterator for &'a Records {
    type Item = (&'a RecordKey, &'a Change);
    type IntoIter = BudgetedSharedMapIter<'a, RecordKey, Change>;
    fn into_iter(self) -> Self::IntoIter {
        self.values.iter()
    }
}
