//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native field membership publishes exact runtime revisions with an export-only durable projection.

use super::FieldAnalyzerBinding;
use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};
use std::{borrow::Borrow, sync::Arc};
use uqa_core::{
    memory::{MemoryBudget, MemoryError, Produced, ProductionControl, RetainedMap},
    CancellationToken, RelationIdentity,
};

mod image;
mod input;
pub use image::CapturedFieldAnalyzers;
use image::ImageBinding;

type Key = (String, String);

struct StoredKey(Produced<Key>);

impl Borrow<Key> for StoredKey {
    fn borrow(&self) -> &Key {
        &self.0
    }
}
impl PartialEq for StoredKey {
    fn eq(&self, other: &Self) -> bool {
        *self.0 == *other.0
    }
}
impl Eq for StoredKey {}
impl PartialOrd for StoredKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for StoredKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.cmp(&other.0)
    }
}

impl StoredKey {
    fn copy(key: &Key, control: &ProductionControl<'_>) -> StorageBackendResult<Self> {
        let (table, table_memory) = control.copy_text(&key.0)?.into_parts();
        let (field, field_memory) = control.copy_text(&key.1)?.into_parts();
        Ok(Self(control.finish(
            (table, field),
            control.combine(table_memory, field_memory),
        )?))
    }

    fn bytes(&self) -> StorageBackendResult<usize> {
        self.0
             .0
            .capacity()
            .checked_add(self.0 .1.capacity())
            .ok_or(MemoryError::SizeOverflow.into())
    }
}

#[derive(Clone)]
enum RuntimeBinding {
    Native(FieldAnalyzerBinding),
    Image(Arc<uqa_core::memory::Budgeted<FieldAnalyzerBinding>>),
}

impl std::ops::Deref for RuntimeBinding {
    type Target = FieldAnalyzerBinding;
    fn deref(&self) -> &Self::Target {
        match self {
            Self::Native(value) => value,
            Self::Image(value) => value,
        }
    }
}

type Runtime = RetainedMap<StoredKey, RuntimeBinding>;

/// Only the native owner publishes membership. Temporary entries participate in ordinary runtime lookup, but never prepare or retain an encoded image.
#[derive(Clone)]
pub struct FieldAnalyzerCatalog {
    durable: Runtime,
    temporary: Runtime,
    images: RetainedMap<StoredKey, Arc<Produced<ImageBinding>>>,
}

impl Default for FieldAnalyzerCatalog {
    fn default() -> Self {
        Self::new(&MemoryBudget::new(usize::MAX))
    }
}

impl FieldAnalyzerCatalog {
    pub fn new(memory: &MemoryBudget) -> Self {
        Self {
            durable: RetainedMap::new(memory),
            temporary: RetainedMap::new(memory),
            images: RetainedMap::new(memory),
        }
    }

    pub fn len(&self) -> usize {
        self.durable.len() + self.temporary.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, key: &Key) -> Option<&FieldAnalyzerBinding> {
        self.durable
            .get(key)
            .or_else(|| self.temporary.get(key))
            .map(|value| &**value)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Key, &FieldAnalyzerBinding)> {
        let mut durable = self.durable.iter().peekable();
        let mut temporary = self.temporary.iter().peekable();
        std::iter::from_fn(move || {
            let item = match (durable.peek(), temporary.peek()) {
                (Some((left, _)), Some((right, _))) if left < right => durable.next(),
                (_, Some(_)) => temporary.next(),
                (Some(_), None) => durable.next(),
                (None, None) => None,
            };
            item.map(|(key, value)| (&*key.0, &**value))
        })
    }

    pub fn values(&self) -> impl Iterator<Item = &FieldAnalyzerBinding> {
        self.iter().map(|(_, value)| value)
    }

    /// The resolved relation owner supplies actual persistence before publication. Admission completes for all roots before any of them becomes visible.
    pub fn insert(
        &mut self,
        key: &Key,
        value: FieldAnalyzerBinding,
        temporary: bool,
        cancellation: &CancellationToken,
    ) -> StorageBackendResult<()> {
        cancellation.check()?;
        if RelationIdentity::borrowed_reference(&key.0).is_none() {
            return Err(StorageBackendError::Other(
                "field analyzer has an invalid relation name".into(),
            ));
        }
        let production = ProductionControl::new(self.images.budget(), cancellation, cancellation);
        let runtime_key = StoredKey::copy(key, &production)?;
        let runtime_bytes = runtime_key.bytes()?;
        let mut candidate = self.clone();
        if temporary {
            candidate.durable.remove(key)?;
            candidate.images.remove(key)?;
            candidate.temporary.insert(
                runtime_key,
                RuntimeBinding::Native(value),
                runtime_bytes,
            )?;
        } else {
            let image = ImageBinding::prepare(&value, &production)?;
            let image_key = StoredKey::copy(key, &production)?;
            let bytes = image_key
                .bytes()?
                .checked_add(image.bytes)
                .ok_or(MemoryError::SizeOverflow)?;
            candidate.temporary.remove(key)?;
            candidate.images.insert(image_key, image, bytes)?;
            candidate
                .durable
                .insert(runtime_key, RuntimeBinding::Native(value), runtime_bytes)?;
        }
        cancellation.check()?;
        *self = candidate;
        Ok(())
    }

    pub fn remove(&mut self, key: &Key) -> StorageBackendResult<bool> {
        let mut candidate = self.clone();
        let removed = candidate.durable.remove(key)? | candidate.temporary.remove(key)?;
        candidate.images.remove(key)?;
        *self = candidate;
        Ok(removed)
    }

    pub fn clear_durable(&mut self) {
        self.durable.clear();
        self.images.clear();
    }

    pub fn without_table(&self, table: &str) -> StorageBackendResult<Self> {
        let mut candidate = self.clone();
        let start = (table.to_owned(), String::new());
        for values in [&self.durable, &self.temporary] {
            for (key, _) in values.range_from(std::ops::Bound::Included(&start)) {
                if key.0 .0 != table {
                    break;
                }
                candidate.remove(&key.0)?;
            }
        }
        Ok(candidate)
    }

    pub fn with_renamed_field(
        &self,
        table: &str,
        from: &str,
        to: &str,
    ) -> StorageBackendResult<Self> {
        let key = (table.to_owned(), from.to_owned());
        let mut candidate = self.clone();
        if from != to {
            if let Some(value) = self.get(&key) {
                candidate.insert(
                    &(table.to_owned(), to.to_owned()),
                    value.clone(),
                    self.temporary.get(&key).is_some(),
                    &CancellationToken::new(),
                )?;
                candidate.remove(&key)?;
            }
        }
        Ok(candidate)
    }

    pub fn with_renamed_table(&self, from: &str, to: &str) -> StorageBackendResult<Self> {
        if from == to {
            return Ok(self.clone());
        }
        let mut candidate = self.clone();
        let start = (from.to_owned(), String::new());
        for (values, temporary) in [(&self.durable, false), (&self.temporary, true)] {
            for (key, value) in values.range_from(std::ops::Bound::Included(&start)) {
                if key.0 .0 != from {
                    break;
                }
                candidate.insert(
                    &(to.to_owned(), key.0 .1.clone()),
                    (**value).clone(),
                    temporary,
                    &CancellationToken::new(),
                )?;
                candidate.remove(&key.0)?;
            }
        }
        Ok(candidate)
    }

    /// Cached durable membership admits the full encoded closure without inspecting runtime revisions or temporary entries.
    pub fn capture(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<uqa_core::memory::Budgeted<CapturedFieldAnalyzers>> {
        CapturedFieldAnalyzers::capture(&self.images, control)
    }
}

impl std::ops::Index<&Key> for FieldAnalyzerCatalog {
    type Output = FieldAnalyzerBinding;
    fn index(&self, key: &Key) -> &Self::Output {
        self.get(key)
            .expect("field analyzer exists in this catalog")
    }
}

#[cfg(test)]
mod tests;
