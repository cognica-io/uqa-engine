//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered immutable roots move from charged memory to encrypted temporary pages.

mod disk;
mod node;
#[cfg(test)]
mod tests;

use std::{ops::Deref, sync::Arc};
use uqa_core::memory::{Budgeted, BudgetedSharedMap, MemoryBudget, MemoryError, MemoryReservation};

use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};

pub(super) trait Record: Sized {
    fn memory_bytes(&self) -> StorageBackendResult<usize>;
    fn encoded_bytes(&self) -> StorageBackendResult<usize>;
    fn encode(&self, bytes: &mut Vec<u8>);
    fn decode(bytes: &[u8]) -> StorageBackendResult<Self>;
}

pub(super) enum Read<'a, V> {
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
pub(super) struct Map<V> {
    root: Root<V>,
    len: usize,
    memory: MemoryBudget,
    resident_bytes: usize,
}

impl<V> Clone for Map<V> {
    fn clone(&self) -> Self {
        Self {
            root: self.root.clone(),
            len: self.len,
            memory: self.memory.clone(),
            resident_bytes: self.resident_bytes,
        }
    }
}

impl<V> std::fmt::Debug for Map<V> {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("HNSWMap")
            .field("len", &self.len)
            .field("spilled", &matches!(self.root, Root::Disk(_)))
            .finish_non_exhaustive()
    }
}

impl<V: Record> Map<V> {
    pub(super) fn new(memory: &MemoryBudget, resident_bytes: usize) -> Self {
        Self {
            root: Root::Memory(BudgetedSharedMap::new(&memory.child(resident_bytes))),
            len: 0,
            memory: memory.clone(),
            resident_bytes,
        }
    }

    pub(super) fn len(&self) -> usize {
        self.len
    }

    pub(super) fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub(super) fn clear(&mut self) {
        *self = Self::new(&self.memory, self.resident_bytes);
    }

    pub(super) fn iter(&self) -> Iter<'_, V> {
        Iter {
            map: self,
            after: None,
            done: false,
        }
    }

    pub(super) fn get(&self, key: u128) -> StorageBackendResult<Option<Read<'_, V>>> {
        match &self.root {
            Root::Memory(map) => Ok(map
                .get(&key)
                .and_then(Option::as_ref)
                .map(|value| Read::Borrowed(&***value))),
            Root::Disk(map) => map
                .get(key, &self.memory)
                .map(|value| value.map(Read::Owned)),
        }
    }

    pub(super) fn next(
        &self,
        after: Option<u128>,
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
                .next(after, &self.memory)
                .map(|entry| entry.map(|(key, value)| (key, Read::Owned(value)))),
        }
    }

    pub(super) fn insert(
        &mut self,
        key: u128,
        value: V,
        control: Option<&StorageReadControl>,
    ) -> StorageBackendResult<()> {
        super::prepare::check(control)?;
        let present = self.get(key)?.is_some();
        let mut value = Some(value);
        let mut retained = None;
        if let Root::Memory(map) = &mut self.root {
            let bytes = value
                .as_ref()
                .expect("unmoved value")
                .memory_bytes()?
                .checked_add(size_of::<MemoryReservation>())
                .ok_or(MemoryError::SizeOverflow)?;
            match map.budget().reserve(bytes) {
                Ok(memory) => {
                    let value =
                        Arc::new(Budgeted::new(value.take().expect("unmoved value"), memory));
                    match map.try_insert(key, Some(Arc::clone(&value))) {
                        Ok(()) => {
                            self.len += usize::from(!present);
                            return Ok(());
                        }
                        Err(MemoryError::Limit { .. }) => retained = Some(value),
                        Err(error) => return Err(error.into()),
                    }
                }
                Err(MemoryError::Limit { .. }) => {
                    self.spill(control)?;
                    return self.insert_disk(
                        key,
                        value.as_ref().expect("unmoved value"),
                        present,
                        control,
                    );
                }
                Err(error) => return Err(error.into()),
            }
        }
        if let Some(retained) = retained {
            self.spill(control)?;
            self.insert_disk(key, &retained, present, control)
        } else {
            self.insert_disk(
                key,
                value.as_ref().expect("unmoved value"),
                present,
                control,
            )
        }
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
            super::prepare::check(control)?;
            *map = candidate;
            self.len += usize::from(!present);
            Ok(())
        } else {
            unreachable!("resident insertion either succeeded or moved to disk")
        }
    }

    pub(super) fn remove(
        &mut self,
        key: u128,
        control: Option<&StorageReadControl>,
    ) -> StorageBackendResult<()> {
        super::prepare::check(control)?;
        if self.get(key)?.is_none() {
            return Ok(());
        }
        if let Root::Memory(map) = &mut self.root {
            match map.try_insert(key, None) {
                Ok(()) => {
                    self.len -= 1;
                    return Ok(());
                }
                Err(MemoryError::Limit { .. }) => self.spill(control)?,
                Err(error) => return Err(error.into()),
            }
        }
        if let Root::Disk(map) = &mut self.root {
            let mut candidate = map.clone();
            candidate.remove(key, &self.memory)?;
            candidate.compact::<V>(&self.memory, control)?;
            super::prepare::check(control)?;
            *map = candidate;
            self.len -= 1;
        }
        Ok(())
    }

    pub(super) fn last(&self) -> StorageBackendResult<Option<(u128, Read<'_, V>)>> {
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

    pub(super) fn spill(
        &mut self,
        control: Option<&StorageReadControl>,
    ) -> StorageBackendResult<()> {
        let Root::Memory(resident) = &self.root else {
            return Ok(());
        };
        let mut disk = disk::Builder::new(&self.memory)?;
        for (key, value) in resident {
            super::prepare::check(control)?;
            if let Some(value) = value {
                disk.push(*key, &***value, &self.memory)?;
            }
        }
        let root = disk.finish()?;
        super::prepare::check(control)?;
        self.root = Root::Disk(root);
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn is_spilled(&self) -> bool {
        matches!(self.root, Root::Disk(_))
    }
}

pub(super) struct Iter<'a, V> {
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
    StorageBackendError::backend("HNSW temporary pages", error)
}

fn invalid(message: &'static str) -> StorageBackendError {
    StorageBackendError::Other(format!("invalid HNSW temporary record: {message}"))
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
