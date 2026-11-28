//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical encoded index, disk buckets, match flags, and spill transition.

use crate::distinct::EncodedKey;
use crate::spill::PartitionedKeys;
use crate::{ExecError, ExecResult};
use std::collections::{hash_map::Entry, HashMap};
use std::sync::Arc;
mod cursor;
pub(super) use cursor::HashMatches;

/// Exact, work-memory-bounded build-side hash index. It starts in memory and
/// migrates atomically to a partitioned temporary index before the encoded key and
/// row-index records would exceed its byte budget. Disk probes always compare
/// the full key, so bucket hash collisions cannot create false join matches.
pub(crate) struct HybridHashIndex {
    memory: HashMap<EncodedKey, Arc<Vec<u64>>, ahash::RandomState>,
    memory_bytes: usize,
    budget_bytes: usize,
    disk: Option<Box<PartitionedKeys>>,
}

#[derive(Clone, Copy)]
pub(super) enum MemoryMatchSummary {
    Absent,
    Single(u64),
    Multiple,
}

impl HybridHashIndex {
    pub(crate) fn new(budget_bytes: usize) -> Self {
        Self {
            // AHash keeps a per-index random seed while avoiding SipHash's
            // cryptographic-round overhead after the canonical SQL key has
            // already been encoded byte-for-byte.
            memory: HashMap::with_hasher(ahash::RandomState::new()),
            memory_bytes: 0,
            budget_bytes,
            disk: None,
        }
    }

    pub(crate) fn insert(&mut self, key: EncodedKey, row_index: u64) -> ExecResult<()> {
        if let Some(disk) = self.disk.as_mut() {
            return disk.insert(&key, Some(row_index)).map(|_| ());
        }

        let entry = self.memory.entry(key);
        let record_bytes = entry
            .key()
            .len()
            .checked_add(if matches!(entry, Entry::Occupied(_)) {
                16
            } else {
                16 + std::mem::size_of::<Vec<u64>>() + 2 * std::mem::size_of::<usize>()
            })
            .ok_or_else(|| ExecError::Other("join hash-index size overflow".into()))?;
        if let Some(bytes) = self
            .memory_bytes
            .checked_add(record_bytes)
            .filter(|bytes| *bytes <= self.budget_bytes)
        {
            Arc::get_mut(entry.or_default())
                .ok_or_else(|| {
                    ExecError::Other("join index mutated during an active probe".into())
                })?
                .push(row_index);
            self.memory_bytes = bytes;
            return Ok(());
        }

        let key = match entry {
            Entry::Vacant(entry) => entry.into_key(),
            Entry::Occupied(entry) => entry.key().clone(),
        };
        let mut disk = PartitionedKeys::new(None, "join")?;
        for (existing_key, indices) in &self.memory {
            for index in indices.iter() {
                disk.insert(existing_key, Some(*index))?;
            }
        }
        disk.insert(&key, Some(row_index))?;
        self.memory = HashMap::with_hasher(ahash::RandomState::new());
        self.memory_bytes = 0;
        self.disk = Some(Box::new(disk));
        Ok(())
    }

    pub(crate) fn match_cursor(&mut self, key: EncodedKey) -> ExecResult<HashMatches> {
        let Some(disk) = self.disk.as_mut() else {
            return Ok(self
                .memory
                .get(key.as_slice())
                .map_or_else(HashMatches::empty, |indices| {
                    HashMatches::memory(Arc::clone(indices))
                }));
        };
        Ok(disk
            .positions(&key)?
            .map_or_else(HashMatches::empty, HashMatches::disk))
    }

    /// Summarize a probe without allocation when the index still resides in
    /// memory. Disk-backed indexes return `None` and use the streaming probe.
    pub(super) fn memory_match_summary(&self, key: &[u8]) -> Option<MemoryMatchSummary> {
        if self.disk.is_some() {
            return None;
        }
        Some(
            match self.memory.get(key).map(|indices| indices.as_slice()) {
                None | Some([]) => MemoryMatchSummary::Absent,
                Some([index]) => MemoryMatchSummary::Single(*index),
                Some(_) => MemoryMatchSummary::Multiple,
            },
        )
    }

    pub(super) fn has_spilled(&self) -> bool {
        self.disk.is_some()
    }

    pub(super) fn is_memory_unique(&self) -> bool {
        self.disk.is_none() && self.memory.values().all(|indices| indices.len() == 1)
    }
}

#[cfg(test)]
mod probe_tests {
    use super::*;
    #[test]
    fn duplicate_probe_cursors_resume_independently_in_memory_and_on_disk() {
        for budget in [1, 64 * 1024] {
            let mut index = HybridHashIndex::new(budget);
            for ordinal in 0..128 {
                index
                    .insert(EncodedKey::from_slice(b"same"), ordinal)
                    .unwrap();
            }
            let mut first = index.match_cursor(EncodedKey::from_slice(b"same")).unwrap();
            let mut second = index.match_cursor(EncodedKey::from_slice(b"same")).unwrap();
            for ordinal in 0..128 {
                assert_eq!(first.next().unwrap(), Some(ordinal));
                if ordinal % 2 == 0 {
                    assert_eq!(second.next().unwrap(), Some(ordinal / 2));
                }
            }
            assert_eq!(first.next().unwrap(), None);
            for ordinal in 64..128 {
                assert_eq!(second.next().unwrap(), Some(ordinal));
            }
            assert_eq!(second.next().unwrap(), None);
            assert_eq!(
                index
                    .match_cursor(EncodedKey::from_slice(b"absent"))
                    .unwrap()
                    .next()
                    .unwrap(),
                None
            );
        }
    }
}
