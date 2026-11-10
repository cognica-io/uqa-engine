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
        mut self,
        name: Budgeted<String>,
        reader: &mut dyn std::io::Read,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
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
        let index = PathIndex::read_image(reader, control)?;
        let PathStorage::Memory(paths) = &index.storage else {
            unreachable!("memory image decoder")
        };
        let bytes = name
            .capacity()
            .checked_add(paths.retained_bytes())
            .ok_or(MemoryError::SizeOverflow)?;
        self.indexes.insert(Produced::from(name), index, bytes)?;
        control.check()?;
        Ok(self)
    }

    pub fn into_mutation_budget(self, memory: &MemoryBudget) -> Self {
        Self {
            indexes: self.indexes.into_mutation_budget(memory),
            ..self
        }
    }
}
