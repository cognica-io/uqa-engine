//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Private integer workspaces append fixed-width Patricia paths without corpus conversion or compaction.

use super::disk;
use crate::{read_control::StorageReadControl, StorageBackendResult};
use uqa_core::memory::{BudgetedMap, MemoryError};

/// This owner cannot be cloned or publish a retained root. A fixed resident prefix stays in its original tree; overflow keys enter a separate append-only file without converting that prefix. Each disk update appends at most one leaf and 128 path headers, with no whole-map maintenance inside a scheduled position. The resident tree is capped at 16 KiB, so retirement also has a fixed bound independent of corpus size.
pub(crate) struct Scratch {
    resident: BudgetedMap<u128, u64>,
    disk: Option<disk::Map>,
    control: StorageReadControl,
}

impl Scratch {
    pub(crate) fn new(control: &StorageReadControl) -> StorageBackendResult<Self> {
        control.check()?;
        Ok(Self {
            resident: BudgetedMap::new(
                &control
                    .memory()
                    .child((control.memory().limit() / 16).min(16 << 10)),
            ),
            disk: None,
            control: control.clone(),
        })
    }

    /// Retire a complete private set. Only the capped resident prefix and fixed page caches require traversal; the ciphertext file has no per-record destructor.
    pub(crate) fn clear(&mut self) {
        self.resident = BudgetedMap::new(self.resident.budget());
        self.disk = None;
    }

    pub(crate) fn get(&self, key: u128) -> StorageBackendResult<Option<u64>> {
        self.control.check()?;
        let value = if let Some(value) = self.resident.get(&key) {
            Some(*value)
        } else if let Some(map) = &self.disk {
            map.get::<u64>(key, self.control.memory())?
                .map(|value| *value)
        } else {
            None
        };
        self.control.check()?;
        Ok(value)
    }

    pub(crate) fn insert(&mut self, key: u128, value: u64) -> StorageBackendResult<()> {
        self.control.check()?;
        if let Some(previous) = self.resident.get_mut(&key) {
            *previous = value;
        } else if let Some(map) = &mut self.disk {
            map.insert(key, &value, self.control.memory())?;
        } else {
            match self.resident.insert(key, value) {
                Ok(_) => (),
                Err(MemoryError::Limit { .. }) => {
                    let mut map = disk::Builder::with_workspace(
                        self.control.memory(),
                        None,
                        self.control.file_budget(),
                    )?
                    .finish()?;
                    map.insert(key, &value, self.control.memory())?;
                    self.disk = Some(map);
                }
                Err(error) => return Err(error.into()),
            }
        }
        self.control.check()
    }
}

#[cfg(test)]
mod tests;
