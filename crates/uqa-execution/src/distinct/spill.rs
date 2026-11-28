//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Memory-to-disk transition and exact bucketed spill storage.

#[cfg(test)]
pub(crate) use crate::spill::stable_hash;
use crate::spill::PartitionedKeys;
use crate::{ExecError, ExecResult};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub(crate) struct SeenKeySet {
    memory: HashSet<Vec<u8>, ahash::RandomState>,
    memory_bytes: usize,
    budget_bytes: usize,
    spill_directory: Option<PathBuf>,
    disk: Option<Box<PartitionedKeys>>,
}

impl SeenKeySet {
    pub(crate) fn new(budget_bytes: usize, spill_directory: Option<PathBuf>) -> Self {
        Self {
            memory: HashSet::with_hasher(ahash::RandomState::new()),
            memory_bytes: 0,
            budget_bytes,
            spill_directory,
            disk: None,
        }
    }

    pub(crate) fn insert(&mut self, key: Vec<u8>) -> ExecResult<bool> {
        if let Some(disk) = self.disk.as_mut() {
            return disk.insert(&key, None);
        }
        if let Some(bytes) = self
            .memory_bytes
            .checked_add(key.len())
            .filter(|bytes| *bytes <= self.budget_bytes)
        {
            let inserted = self.memory.insert(key);
            if inserted {
                self.memory_bytes = bytes;
            }
            return Ok(inserted);
        }
        if self.memory.contains(&key) {
            return Ok(false);
        }

        // Build the disk set off to the side. A create/migration/write error
        // leaves the original in-memory set intact and is returned to the
        // execution pipeline; no key is silently forgotten.
        let mut disk = PartitionedKeys::new(self.spill_directory.as_deref(), "DISTINCT")?;
        for existing in &self.memory {
            if !disk.insert(existing, None)? {
                return Err(ExecError::Other(
                    "duplicate key found while migrating DISTINCT state".into(),
                ));
            }
        }
        let inserted = disk.insert(&key, None)?;
        self.memory = HashSet::with_hasher(ahash::RandomState::new());
        self.memory_bytes = 0;
        self.disk = Some(Box::new(disk));
        Ok(inserted)
    }

    pub(crate) fn contains(&mut self, key: &[u8]) -> ExecResult<bool> {
        match self.disk.as_mut() {
            Some(disk) => disk.contains(key),
            None => Ok(self.memory.contains(key)),
        }
    }

    pub(super) fn has_spilled(&self) -> bool {
        self.disk.is_some()
    }

    pub(super) fn in_memory_bytes(&self) -> usize {
        self.memory_bytes
    }

    pub(super) fn spill_path(&self) -> Option<&Path> {
        self.disk.as_deref().map(PartitionedKeys::path)
    }
}
