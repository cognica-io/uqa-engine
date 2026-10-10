//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered immutable roots move from charged memory to encrypted temporary pages.

mod builder;
mod disk;
#[cfg(test)]
mod tests;

pub(crate) use builder::Builder;

use std::{ops::Deref, sync::Arc};
use uqa_core::memory::{Budgeted, BudgetedSharedMap, MemoryBudget, MemoryError, MemoryReservation};

use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};

pub(crate) trait Record: Sized {
    fn memory_bytes(&self) -> StorageBackendResult<usize>;
    fn encoded_bytes(&self) -> StorageBackendResult<usize>;
    fn encode(&self, bytes: &mut Vec<u8>);
    fn decode(bytes: &[u8]) -> StorageBackendResult<Self>;
}

pub(crate) enum Read<'a, V> {
    Borrowed(&'a V),
    Owned(Budgeted<V>),
}

impl<V> Deref for Read<'_, V> {
    type Target = V;
    fn deref(&self) -> &V {
        match self {
            Self::Borrowed(value) => value,
            Self::Owned(value) => value,
        }
    }
}

enum Root<V> {
    Memory(BudgetedSharedMap<u128, Option<Arc<Budgeted<V>>>>),
    Disk(disk::Map),
}

impl<V> Clone for Root<V> {
    fn clone(&self) -> Self {
        match self {
            Self::Memory(map) => Self::Memory(map.clone()),
            Self::Disk(map) => Self::Disk(map.clone()),
        }
    }
}

/// Payloads and resident tree paths share a component allowance under the original owner. Clones keep immutable roots; spilling one root never rewrites another root's records.
pub(crate) struct Map<V> {
    root: Root<V>,
    len: usize,
    memory: MemoryBudget,
    resident_bytes: usize,
    max_encoded_bytes: usize,
}

impl<V> Clone for Map<V> {
    fn clone(&self) -> Self {
        Self {
            root: self.root.clone(),
            len: self.len,
            memory: self.memory.clone(),
            resident_bytes: self.resident_bytes,
            max_encoded_bytes: self.max_encoded_bytes,
        }
    }
}

impl<V> std::fmt::Debug for Map<V> {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("SpillMap")
            .field("len", &self.len)
            .field("spilled", &matches!(self.root, Root::Disk(_)))
            .finish_non_exhaustive()
    }
}

impl<V: Record> Map<V> {
    pub(crate) fn new(memory: &MemoryBudget, resident_bytes: usize) -> Self {
        Self {
            root: Root::Memory(BudgetedSharedMap::new(&memory.child(resident_bytes))),
            len: 0,
            memory: memory.clone(),
            resident_bytes,
            max_encoded_bytes: 0,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::new(&self.memory, self.resident_bytes);
    }

    pub(crate) fn iter(&self) -> Iter<'_, V> {
        Iter {
            map: self,
            after: None,
            done: false,
        }
    }

    pub(crate) fn contains_key(&self, key: u128) -> StorageBackendResult<bool> {
        match &self.root {
            Root::Memory(map) => Ok(map.get(&key).is_some_and(Option::is_some)),
            Root::Disk(map) => map.contains_key(key),
        }
    }

    pub(crate) fn get(&self, key: u128) -> StorageBackendResult<Option<Read<'_, V>>> {
        self.get_with_memory(key, &self.memory)
    }

    pub(crate) fn get_with_memory(
        &self,
        key: u128,
        memory: &MemoryBudget,
    ) -> StorageBackendResult<Option<Read<'_, V>>> {
        match &self.root {
            Root::Memory(map) => Ok(map
                .get(&key)
                .and_then(Option::as_ref)
                .map(|value| Read::Borrowed(&***value))),
            Root::Disk(map) => map.get(key, memory).map(|value| value.map(Read::Owned)),
        }
    }

    pub(crate) fn next(
        &self,
        after: Option<u128>,
    ) -> StorageBackendResult<Option<(u128, Read<'_, V>)>> {
        self.next_with_memory(after, &self.memory)
    }

    pub(crate) fn next_with_memory(
        &self,
        after: Option<u128>,
        memory: &MemoryBudget,
    ) -> StorageBackendResult<Option<(u128, Read<'_, V>)>> {
        match &self.root {
            Root::Memory(map) => Ok(map
                .range_from(
                    after
                        .as_ref()
                        .map_or(std::ops::Bound::Unbounded, std::ops::Bound::Excluded),
                )
                .find_map(|(key, value)| {
                    value
                        .as_ref()
                        .map(|value| (*key, Read::Borrowed(&***value)))
                })),
            Root::Disk(map) => map
                .next(after, memory)
                .map(|entry| entry.map(|(key, value)| (key, Read::Owned(value)))),
        }
    }

    /// Enumerate metadata without decoding a spilled payload.
    pub(crate) fn next_key(
        &self,
        after: Option<u128>,
        memory: &MemoryBudget,
    ) -> StorageBackendResult<Option<u128>> {
        match &self.root {
            Root::Memory(map) => Ok(map
                .range_from(
                    after
                        .as_ref()
                        .map_or(std::ops::Bound::Unbounded, std::ops::Bound::Excluded),
                )
                .find_map(|(key, value)| value.as_ref().map(|_| *key))),
            Root::Disk(map) => map.next_key(after, memory),
        }
    }

    pub(crate) fn insert(
        &mut self,
        key: u128,
        value: V,
        control: Option<&StorageReadControl>,
    ) -> StorageBackendResult<()> {
        check(control)?;
        let present = self.contains_key(key)?;
        let Some(value) = self.insert_resident(key, value)? else {
            self.len += usize::from(!present);
            return Ok(());
        };
        self.spill(control)?;
        self.insert_disk(key, &value, present, control)
    }

    /// Return ownership when the resident tree cannot admit the value. Both a
    /// live mutation and an unpublished ordered builder use the same allowance.
    fn insert_resident(&mut self, key: u128, value: V) -> StorageBackendResult<Option<V>> {
        let encoded_bytes = self.max_encoded_bytes.max(value.encoded_bytes()?);
        let _workspace = if matches!(self.root, Root::Memory(_)) {
            match self.reserve_spill_workspace(encoded_bytes) {
                Ok(workspace) => workspace,
                Err(StorageBackendError::Memory(MemoryError::Limit { .. })) => {
                    return Ok(Some(value));
                }
                Err(error) => return Err(error),
            }
        } else {
            return Ok(Some(value));
        };
        if let Root::Memory(map) = &mut self.root {
            let bytes = value
                .memory_bytes()?
                .checked_add(size_of::<MemoryReservation>())
                .ok_or(MemoryError::SizeOverflow)?;
            match map.budget().reserve(bytes) {
                Ok(memory) => {
                    let value = Arc::new(Budgeted::new(value, memory));
                    match map.try_insert(key, Some(Arc::clone(&value))) {
                        Ok(()) => {
                            self.max_encoded_bytes = encoded_bytes;
                            return Ok(None);
                        }
                        Err(MemoryError::Limit { .. }) => {
                            let value = match Arc::try_unwrap(value) {
                                Ok(value) => value.into_parts().0,
                                Err(_) => unreachable!("failed tree insertion retains no value"),
                            };
                            return Ok(Some(value));
                        }
                        Err(error) => return Err(error.into()),
                    }
                }
                Err(MemoryError::Limit { .. }) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(Some(value))
    }

    /// A successful resident growth leaves enough shared allowance to encode
    /// its largest record and convert the complete immutable root. A budget
    /// smaller than the conversion workspace can only hold a resident map.
    fn reserve_spill_workspace(
        &self,
        encoded_bytes: usize,
    ) -> StorageBackendResult<Option<MemoryReservation>> {
        let bytes = disk::Builder::workspace_bytes(&self.memory)
            .checked_add(encoded_bytes)
            .ok_or(MemoryError::SizeOverflow)?;
        if bytes > self.memory.limit() {
            return Ok(None);
        }
        Ok(Some(self.memory.reserve(bytes)?))
    }

    fn insert_disk(
        &mut self,
        key: u128,
        value: &V,
        present: bool,
        control: Option<&StorageReadControl>,
    ) -> StorageBackendResult<()> {
        if let Root::Disk(map) = &mut self.root {
            let mut candidate = map.clone();
            candidate.insert(key, value, &self.memory)?;
            candidate.compact::<V>(&self.memory, control)?;
            check(control)?;
            *map = candidate;
            self.len += usize::from(!present);
            Ok(())
        } else {
            unreachable!("resident insertion either succeeded or moved to disk")
        }
    }

    pub(crate) fn remove(
        &mut self,
        key: u128,
        control: Option<&StorageReadControl>,
    ) -> StorageBackendResult<()> {
        check(control)?;
        if !self.contains_key(key)? {
            return Ok(());
        }
        let workspace = if matches!(self.root, Root::Memory(_)) {
            match self.reserve_spill_workspace(self.max_encoded_bytes) {
                Ok(workspace) => workspace,
                Err(StorageBackendError::Memory(MemoryError::Limit { .. })) => {
                    self.spill(control)?;
                    None
                }
                Err(error) => return Err(error),
            }
        } else {
            None
        };
        if let Root::Memory(map) = &mut self.root {
            match map.try_insert(key, None) {
                Ok(()) => {
                    self.len -= 1;
                    return Ok(());
                }
                Err(MemoryError::Limit { .. }) => {
                    drop(workspace);
                    self.spill(control)?;
                }
                Err(error) => return Err(error.into()),
            }
        }
        if let Root::Disk(map) = &mut self.root {
            let mut candidate = map.clone();
            candidate.remove(key, &self.memory)?;
            candidate.compact::<V>(&self.memory, control)?;
            check(control)?;
            *map = candidate;
            self.len -= 1;
        }
        Ok(())
    }

    pub(crate) fn last(&self) -> StorageBackendResult<Option<(u128, Read<'_, V>)>> {
        match &self.root {
            Root::Memory(map) => {
                let mut before = None;
                while let Some((key, value)) = map.last_before(
                    before
                        .as_ref()
                        .map_or(std::ops::Bound::Unbounded, std::ops::Bound::Excluded),
                ) {
                    if let Some(value) = value {
                        return Ok(Some((*key, Read::Borrowed(&***value))));
                    }
                    before = Some(*key);
                }
                Ok(None)
            }
            Root::Disk(map) => map
                .last(&self.memory)
                .map(|entry| entry.map(|(key, value)| (key, Read::Owned(value)))),
        }
    }

    pub(crate) fn spill(
        &mut self,
        control: Option<&StorageReadControl>,
    ) -> StorageBackendResult<()> {
        let Root::Memory(resident) = &self.root else {
            return Ok(());
        };
        let mut disk = disk::Builder::new(&self.memory)?;
        for (key, value) in resident {
            check(control)?;
            if let Some(value) = value {
                disk.push(*key, &***value, &self.memory)?;
            }
        }
        let root = disk.finish()?;
        check(control)?;
        self.root = Root::Disk(root);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn is_spilled(&self) -> bool {
        matches!(self.root, Root::Disk(_))
    }

    #[cfg(test)]
    pub(crate) fn written_bytes(&self) -> u64 {
        match &self.root {
            Root::Memory(_) => 0,
            Root::Disk(map) => map.written_bytes(),
        }
    }
}

pub(crate) struct Iter<'a, V> {
    map: &'a Map<V>,
    after: Option<u128>,
    done: bool,
}

impl<'a, V: Record> Iterator for Iter<'a, V> {
    type Item = StorageBackendResult<(u128, Read<'a, V>)>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.map.next(self.after) {
            Ok(Some((key, value))) => {
                self.after = Some(key);
                Some(Ok((key, value)))
            }
            Ok(None) => {
                self.done = true;
                None
            }
            Err(error) => {
                self.done = true;
                Some(Err(error))
            }
        }
    }
}

fn io(error: std::io::Error) -> StorageBackendError {
    StorageBackendError::backend("temporary ordered pages", error)
}

pub(crate) fn invalid(message: &'static str) -> StorageBackendError {
    StorageBackendError::Other(format!("invalid temporary ordered record: {message}"))
}

impl Record for u64 {
    fn memory_bytes(&self) -> StorageBackendResult<usize> {
        Ok(size_of::<Self>())
    }
    fn encoded_bytes(&self) -> StorageBackendResult<usize> {
        Ok(8)
    }
    fn encode(&self, bytes: &mut Vec<u8>) {
        bytes.extend_from_slice(&self.to_le_bytes());
    }
    fn decode(bytes: &[u8]) -> StorageBackendResult<Self> {
        Ok(Self::from_le_bytes(
            bytes.try_into().map_err(|_| invalid("integer width"))?,
        ))
    }
}

fn check(control: Option<&StorageReadControl>) -> StorageBackendResult<()> {
    if let Some(control) = control {
        control.check()?;
    }
    Ok(())
}
