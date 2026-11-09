//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Admitted evaluated scalar keys use the existing physical order and predicate interface.

use super::{entries::Entries, BTreeIndex};
use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};
use uqa_core::{memory::Budgeted, DocId, Value};

/// A private physical index under construction. Only completion exposes ordinary mutation; rejection consumes every partial field, key and identity buffer.
pub struct BTreeIndexImageBuilder {
    index: BTreeIndex,
    control: StorageReadControl,
}

impl BTreeIndexImageBuilder {
    /// Start a private ordered image from an admitted original field. Later ordinary mutations own their new allocations independently of this input allowance.
    pub fn new(
        field: Budgeted<String>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        if !field.budget().shares_allowance(control.memory())
            || field.reserved_bytes() < field.capacity()
        {
            return Err(invalid("index field has no complete input admission"));
        }
        Ok(Self {
            index: BTreeIndex {
                field: super::field::IndexField::Admitted(field.into_shared()?),
                entries: Entries::admitted(),
            },
            control: control.clone(),
        })
    }

    /// Adopt an evaluated key without SQL comparison. Repeated physical keys append strictly increasing identities; the enclosing image validates global identity uniqueness and its declared count before publication.
    pub fn add_value(mut self, id: DocId, value: Budgeted<Value>) -> StorageBackendResult<Self> {
        self.control.check()?;
        if !value.budget().shares_allowance(self.control.memory()) {
            return Err(invalid("index key belongs to another input allowance"));
        }
        let payload = value
            .retained_payload_bytes(self.control.memory(), self.control.cancellation())
            .map_err(|error| StorageBackendError::backend("evaluated index image", error))?;
        if value.reserved_bytes() < payload {
            return Err(invalid("index key has no complete input admission"));
        }
        self.index.entries.insert_admitted(id, value)?;
        self.control.check()?;
        Ok(self)
    }

    pub fn finish(self) -> StorageBackendResult<BTreeIndex> {
        self.control.check()?;
        Ok(self.index)
    }
}

fn invalid(message: &'static str) -> StorageBackendError {
    StorageBackendError::Other(message.into())
}

#[cfg(test)]
mod tests;
