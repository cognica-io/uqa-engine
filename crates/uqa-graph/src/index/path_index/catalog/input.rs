//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluated path input adopts names and immutable pairs without graph evaluation.

use super::{PathIndex, PathIndexCatalog, PathStorage};
use uqa_core::memory::{Budgeted, MemoryBudget, MemoryError, Produced};
use uqa_storage::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};

impl PathIndexCatalog {
    pub fn read_image(
        self,
        name: Budgeted<String>,
        reader: &mut dyn std::io::Read,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        uqa_core::image_io::decode_image(
            reader,
            control.memory(),
            control.cancellation(),
            async |mut input| self.read_image_from(name, &mut input).await,
        )
    }

    /// Consume the private catalog through complete native input and publication. The caller forwards the original input channel; failure cannot expose or reuse a partially updated catalog.
    pub async fn read_image_from(
        mut self,
        name: Budgeted<String>,
        input: &mut uqa_core::image_io::AsyncImageInput<'_>,
    ) -> StorageBackendResult<Self> {
        let control = StorageReadControl::new(input.memory, input.cancellation);
        input.checkpoint().await?;
        control.check()?;
        if !name.budget().shares_allowance(control.memory())
            || !self.indexes.budget().shares_allowance(control.memory())
            || name.reserved_bytes() < name.capacity()
            || self.contains_key(name.as_str())
        {
            return Err(StorageBackendError::Other(
                "invalid path image admission or identity".into(),
            ));
        }
        let index = PathIndex::read_image_from(input).await?;
        let PathStorage::Memory(paths) = &index.storage else {
            unreachable!("memory image decoder")
        };
        let bytes = name
            .capacity()
            .checked_add(paths.retained_bytes())
            .ok_or(MemoryError::SizeOverflow)?;
        self.indexes.insert(Produced::from(name), index, bytes)?;
        input.checkpoint().await?;
        Ok(self)
    }

    pub fn into_mutation_budget(self, memory: &MemoryBudget) -> Self {
        Self {
            indexes: self.indexes.into_mutation_budget(memory),
            ..self
        }
    }
}
