//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Named membership and evaluated index state publish separate immutable source identities.

use super::{CapturedValueIndex, ValueIndexCache};
use std::collections::BTreeMap;
use uqa_core::memory::{MemoryBudget, OwnedMap};
use uqa_storage::{
    memory_capture::{MemoryCaptureCell, MemoryCaptureRegistry},
    StorageBackendResult, ValueIndexKey,
};

mod capture;
mod guards;
mod image;
mod native;
pub use capture::CapturedValueIndexes;
pub use guards::{ValueIndexRead, ValueIndexWrite};
pub use image::MemoryValueIndexesBuilder;
use native::NativeValueSources;

enum Entry {
    Plain(ValueIndexCache),
    Captured(MemoryCaptureCell<ValueIndexCache>),
}

impl Entry {
    fn read(&self) -> ValueIndexRead<'_> {
        match self {
            Self::Plain(value) => ValueIndexRead::Plain(value),
            Self::Captured(value) => ValueIndexRead::Captured(value.read()),
        }
    }

    fn write(&mut self) -> ValueIndexWrite<'_> {
        match self {
            Self::Plain(value) => ValueIndexWrite::Plain(value),
            Self::Captured(value) => ValueIndexWrite::Captured(value.write()),
        }
    }
}

/// The enclosing table source owns collection admission. Named children preserve their own evaluated roots before mutation or retirement; derived column caches remain ordinary query state.
#[derive(Default)]
pub struct ValueIndexes {
    indexes: OwnedMap<ValueIndexKey, Entry>,
    native: Option<NativeValueSources>,
}

impl From<BTreeMap<ValueIndexKey, ValueIndexCache>> for ValueIndexes {
    fn from(indexes: BTreeMap<ValueIndexKey, ValueIndexCache>) -> Self {
        Self {
            indexes: indexes
                .into_iter()
                .map(|(key, value)| (key, Entry::Plain(value)))
                .collect(),
            native: None,
        }
    }
}

impl ValueIndexes {
    pub fn memory(registry: &MemoryCaptureRegistry) -> Self {
        Self::memory_with_budget(registry, &MemoryBudget::new(usize::MAX))
    }

    pub fn memory_with_budget(registry: &MemoryCaptureRegistry, memory: &MemoryBudget) -> Self {
        Self {
            indexes: OwnedMap::new(),
            native: Some(NativeValueSources::new(registry, memory)),
        }
    }

    pub fn len(&self) -> usize {
        self.indexes.len()
    }
    pub fn is_empty(&self) -> bool {
        self.indexes.is_empty()
    }
    pub fn contains_key(&self, key: &ValueIndexKey) -> bool {
        self.indexes.contains_key(key)
    }
    pub fn get(&self, key: &ValueIndexKey) -> Option<ValueIndexRead<'_>> {
        self.indexes.get(key).map(Entry::read)
    }
    pub fn get_mut(&mut self, key: &ValueIndexKey) -> Option<ValueIndexWrite<'_>> {
        self.indexes.get_mut(key).map(Entry::write)
    }
    pub fn keys(&self) -> impl ExactSizeIterator<Item = &ValueIndexKey> {
        self.indexes.keys()
    }
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&ValueIndexKey, ValueIndexRead<'_>)> {
        self.indexes.iter().map(|(key, value)| (key, value.read()))
    }
    pub fn values(&self) -> impl ExactSizeIterator<Item = ValueIndexRead<'_>> {
        self.indexes.values().map(Entry::read)
    }
    pub fn values_mut(&mut self) -> impl ExactSizeIterator<Item = ValueIndexWrite<'_>> {
        self.indexes.values_mut().map(Entry::write)
    }

    pub fn iter_mut(
        &mut self,
    ) -> impl ExactSizeIterator<Item = (&ValueIndexKey, ValueIndexWrite<'_>)> {
        self.indexes
            .iter_mut()
            .map(|(key, value)| (key, value.write()))
    }

    pub fn insert_if_absent(
        &mut self,
        key: ValueIndexKey,
        value: ValueIndexCache,
    ) -> StorageBackendResult<()> {
        if !self.contains_key(&key) {
            self.insert(key, value)?;
        }
        Ok(())
    }

    pub fn retain(
        &mut self,
        mut keep: impl FnMut(&ValueIndexKey) -> bool,
    ) -> StorageBackendResult<()> {
        let removed: std::collections::BTreeSet<_> =
            self.keys().filter(|key| !keep(key)).cloned().collect();
        let mut next = self.native.clone();
        if let Some(native) = &mut next {
            for key in &removed {
                *native = native.without_key(key)?;
            }
            self.native
                .as_ref()
                .expect("native successor")
                .publish_all(native)?;
        }
        self.indexes.retain(|key, _| !removed.contains(key));
        self.native = next;
        Ok(())
    }

    /// Undo retains independent writable cache values; publication in a capture domain assigns those owners fresh child identities.
    pub fn snapshot(&self) -> BTreeMap<ValueIndexKey, ValueIndexCache> {
        self.iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }

    pub fn restore(&mut self, source: &Self) -> StorageBackendResult<()> {
        self.replace(source.snapshot())
    }

    pub fn insert(
        &mut self,
        key: ValueIndexKey,
        value: ValueIndexCache,
    ) -> StorageBackendResult<()> {
        let entry = match &self.native {
            Some(native) if matches!(key, ValueIndexKey::Index(_)) => {
                Entry::Captured(native.registry.cell(value)?)
            }
            _ => Entry::Plain(value),
        };
        let next = self
            .native
            .as_ref()
            .map(|native| native.with_entry(&key, &entry))
            .transpose()?;
        if let (Some(previous), Some(next)) = (&self.native, &next) {
            previous.publish_key(&key, next)?;
        }
        // Dropping the old cell preserves every previously selected child before releasing its source owner.
        self.indexes.insert(key, entry);
        self.native = next;
        Ok(())
    }

    pub fn remove(&mut self, key: &ValueIndexKey) -> StorageBackendResult<bool> {
        if !self.indexes.contains_key(key) {
            return Ok(false);
        }
        let next = self
            .native
            .as_ref()
            .map(|native| native.without_key(key))
            .transpose()?;
        if let (Some(previous), Some(next)) = (&self.native, &next) {
            previous.publish_key(key, next)?;
        }
        self.indexes.remove(key);
        self.native = next;
        Ok(true)
    }

    pub fn clear(&mut self) -> StorageBackendResult<()> {
        self.replace(BTreeMap::new())
    }

    /// Register and admit the complete successor before replacing any live membership or child. Failed preparation leaves the current collection and publication group unchanged.
    pub fn replace(
        &mut self,
        indexes: BTreeMap<ValueIndexKey, ValueIndexCache>,
    ) -> StorageBackendResult<()> {
        let mut next = self.native.as_ref().map_or_else(Self::default, |native| {
            Self::memory_with_budget(&native.registry, native.names.budget())
        });
        for (key, value) in indexes {
            next.insert(key, value)?;
        }
        if let (Some(previous), Some(native)) = (&self.native, &mut next.native) {
            previous.publish_all(native)?;
        }
        *self = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
