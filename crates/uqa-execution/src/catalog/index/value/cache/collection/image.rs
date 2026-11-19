//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Named evaluated indexes adopt decoded registration ownership before table publication.

use super::{Entry, NativeValueSources, ValueIndexCache, ValueIndexes};
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{Budgeted, MemoryBudget, OwnedMap},
};
use uqa_storage::{
    memory_capture::MemoryCaptureRegistry, read_control::StorageReadControl, StorageBackendError,
    StorageBackendResult, ValueIndexKey,
};

/// An unpublished collection of named evaluated indexes. Derived column caches are outside backup membership. A failed addition consumes the partial collection and every original name reservation.
pub struct MemoryValueIndexesBuilder {
    indexes: OwnedMap<ValueIndexKey, Entry>,
    native: NativeValueSources,
    control: StorageReadControl,
    selected: Option<input::Selected>,
}

impl MemoryValueIndexesBuilder {
    pub fn new(registry: &MemoryCaptureRegistry, control: &StorageReadControl) -> Self {
        Self {
            indexes: OwnedMap::new(),
            native: NativeValueSources::new(registry, control.memory()),
            control: control.clone(),
            selected: None,
        }
    }

    pub fn add_index(
        self,
        name: Budgeted<String>,
        value: ValueIndexCache,
    ) -> StorageBackendResult<Self> {
        let (key, entry) = self.prepare_index(name, value)?;
        let next = self.native.with_entry(&key, &entry)?;
        self.publish_index(key, entry, next)
    }

    async fn add_index_from(
        self,
        name: Budgeted<String>,
        value: ValueIndexCache,
        input: &mut AsyncImageInput<'_>,
    ) -> StorageBackendResult<Self> {
        input.checkpoint().await?;
        let (key, entry) = self.prepare_index(name, value)?;
        let next = self.native.with_entry_from(&key, &entry, input).await?;
        self.publish_index(key, entry, next)
    }

    fn prepare_index(
        &self,
        name: Budgeted<String>,
        value: ValueIndexCache,
    ) -> StorageBackendResult<(Budgeted<ValueIndexKey>, Entry)> {
        self.control.check()?;
        if self.selected.is_some() {
            return Err(invalid("evaluated index is missing declared values"));
        }
        if !name.budget().shares_allowance(self.control.memory())
            || name.reserved_bytes() < name.capacity()
        {
            return Err(invalid(
                "evaluated index name has no complete input admission",
            ));
        }
        let (name, memory) = name.into_parts();
        let key = Budgeted::new(ValueIndexKey::Index(name), memory);
        if self.indexes.contains_key(&*key) {
            return Err(invalid("evaluated index image repeats a registration"));
        }
        let entry = Entry::Captured(
            self.native
                .registry
                .cell_with_control(value, &self.control)?,
        );
        Ok((key, entry))
    }

    fn publish_index(
        mut self,
        key: Budgeted<ValueIndexKey>,
        entry: Entry,
        next: NativeValueSources,
    ) -> StorageBackendResult<Self> {
        self.indexes.insert_admitted_key(key, entry)?;
        self.native = next;
        self.control.check()?;
        Ok(self)
    }

    pub fn finish(self) -> StorageBackendResult<ValueIndexes> {
        self.control.check()?;
        if self.selected.is_some() {
            return Err(invalid("evaluated index is missing declared values"));
        }
        Ok(ValueIndexes {
            indexes: self.indexes,
            native: Some(
                self.native
                    .into_mutation_budget(&MemoryBudget::new(usize::MAX)),
            ),
        })
    }
}

pub(super) fn invalid(message: &'static str) -> StorageBackendError {
    StorageBackendError::Other(message.into())
}

mod input;

#[cfg(test)]
mod tests;
