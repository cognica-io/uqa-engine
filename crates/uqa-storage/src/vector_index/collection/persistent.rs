//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistent vector registrations preserve original field, node and writable handle owners.

use super::{Contents, NativeIndexes, VectorIndex, VectorIndexes};
use crate::{
    read_control::StorageReadControl, StorageBackendError, StorageBackendResult, StorageHandle,
};
use uqa_core::memory::Budgeted;

mod catalog;

/// Assemble an unpublished writable collection from admitted physical handles. Failed additions consume the whole candidate. Completion preserves original leases, while later mutations use their own controls independently of construction cancellation.
pub struct PersistentVectorIndexesBuilder {
    indexes: NativeIndexes,
    control: StorageReadControl,
}

impl PersistentVectorIndexesBuilder {
    pub fn new(control: &StorageReadControl) -> Self {
        Self {
            indexes: NativeIndexes::new(),
            control: control.clone(),
        }
    }

    /// Adopt a unique original field and its controlled physical owner without copying either allocation.
    pub fn add_index(
        self,
        field: Budgeted<String>,
        index: StorageHandle<dyn VectorIndex>,
    ) -> StorageBackendResult<Self> {
        self.insert_index(field, index, true)
    }

    fn insert_index(
        mut self,
        field: Budgeted<String>,
        index: StorageHandle<dyn VectorIndex>,
        unique: bool,
    ) -> StorageBackendResult<Self> {
        self.control.check()?;
        if !field.budget().shares_allowance(self.control.memory())
            || field.reserved_bytes() < field.capacity()
        {
            return Err(invalid("vector field has no complete input admission"));
        }
        if !index.is_retained_by(self.control.memory()) {
            return Err(invalid("vector handle has no complete input admission"));
        }
        if unique && self.indexes.contains_key(field.as_str()) {
            return Err(invalid(
                "persistent vector catalog repeats a field registration",
            ));
        }
        self.indexes.insert_admitted_key(field, index)?;
        self.control.check()?;
        Ok(self)
    }

    pub fn finish(self) -> StorageBackendResult<VectorIndexes> {
        self.control.check()?;
        Ok(VectorIndexes(Contents::Live {
            indexes: self.indexes,
            capture: None,
        }))
    }
}

fn invalid(message: &'static str) -> StorageBackendError {
    StorageBackendError::Other(message.into())
}

#[cfg(test)]
mod tests;
