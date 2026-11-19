//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Primary immutable reachability owns ordered sequences, pairs and their publication-time footprint.

use super::{CapturedPath, CapturedPathIndex, Pairs};
use std::collections::BTreeMap;
use uqa_core::memory::{MemoryError, MemoryReservation, ProductionControl, ProductionVec};
use uqa_storage::StorageBackendResult;

pub(super) struct MemoryPaths {
    pub(super) image: CapturedPathIndex,
    bytes: usize,
    // Imported native payloads retain admission through every shared generation.
    _memory: Option<MemoryReservation>,
}

impl Default for MemoryPaths {
    fn default() -> Self {
        Self {
            image: CapturedPathIndex { paths: Vec::new() },
            bytes: size_of::<Self>(),
            _memory: None,
        }
    }
}

impl MemoryPaths {
    pub(super) fn from_entries(
        entries: BTreeMap<Vec<String>, Pairs>,
    ) -> StorageBackendResult<Self> {
        let production = ProductionControl::uncontrolled();
        let mut paths = ProductionVec::new(production);
        paths.reserve(entries.len())?;
        for (sequence, pairs) in entries {
            let mut ordered = ProductionVec::new(production);
            ordered.reserve(pairs.len())?;
            for pair in pairs {
                ordered.push_copy(pair)?;
            }
            let (pairs, memory) = ordered.finish()?.into_parts();
            paths.push_produced(production.finish(CapturedPath { sequence, pairs }, memory)?)?;
        }
        let (paths, _) = paths.finish()?.into_parts();
        let mut bytes = size_of::<Self>();
        add_capacity::<CapturedPath>(&mut bytes, paths.capacity())?;
        for path in &paths {
            add_capacity::<String>(&mut bytes, path.sequence.capacity())?;
            add_capacity::<(u64, u64)>(&mut bytes, path.pairs.capacity())?;
            for label in &path.sequence {
                add_capacity::<u8>(&mut bytes, label.capacity())?;
            }
        }
        Ok(Self {
            image: CapturedPathIndex { paths },
            bytes,
            _memory: None,
        })
    }

    pub(super) fn from_admitted(image: CapturedPathIndex, memory: MemoryReservation) -> Self {
        Self {
            image,
            bytes: memory.bytes(),
            _memory: Some(memory),
        }
    }

    pub(super) fn get(&self, sequence: &[String]) -> Option<&[(u64, u64)]> {
        self.image
            .paths
            .binary_search_by(|path| path.sequence.as_slice().cmp(sequence))
            .ok()
            .map(|index| self.image.paths[index].pairs.as_slice())
    }

    pub(super) fn retained_bytes(&self) -> usize {
        self.bytes
    }
}

fn add_capacity<T>(bytes: &mut usize, capacity: usize) -> Result<(), MemoryError> {
    *bytes = capacity
        .checked_mul(size_of::<T>())
        .and_then(|extra| bytes.checked_add(extra))
        .ok_or(MemoryError::SizeOverflow)?;
    Ok(())
}
