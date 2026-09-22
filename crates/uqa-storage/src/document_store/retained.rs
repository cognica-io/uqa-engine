//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable decoded fields keep their payload reservation through every retained reader.

use super::{Arc, Document};
use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};
use uqa_core::{
    memory::{Budgeted, MemoryError},
    FieldName, Value, ValueRetentionError,
};

/// One adopted field map and its shared payload charge. Caller-owned input and read outputs retain their own allocation responsibility; clones of this handle retain one lease. Opaque B-tree node slack and allocator bookkeeping are outside the decoded payload allowance.
#[derive(Clone, Debug)]
pub struct RetainedDocumentFields(Arc<Budgeted<Arc<Document>>>);

impl RetainedDocumentFields {
    pub fn new(fields: Arc<Document>, control: &StorageReadControl) -> StorageBackendResult<Self> {
        control.check()?;
        let entries = fields
            .len()
            .checked_mul(size_of::<(FieldName, Value)>())
            .ok_or(MemoryError::SizeOverflow)?;
        let mut memory = control.memory().reserve(size_of::<Document>())?;
        memory.grow(entries)?;
        for (name, value) in fields.iter() {
            control.check()?;
            memory.grow(name.capacity())?;
            let payload = value
                .reserve_retained_payload(control.memory(), control.cancellation())
                .map_err(|error| match error {
                    ValueRetentionError::Memory(error) => StorageBackendError::Memory(error),
                    ValueRetentionError::Cancelled(error) => StorageBackendError::Cancelled(error),
                })?;
            memory.absorb(payload);
        }
        control.check()?;
        Ok(Self(Budgeted::new(fields, memory).into_shared()?))
    }

    /// Transfer uniquely owned fields without copying their values. The returned document is caller-owned; retained siblings keep their original reservation.
    pub fn into_document(self) -> Document {
        match Arc::try_unwrap(self.0) {
            Ok(fields) => {
                let (fields, _memory) = fields.into_parts();
                Arc::unwrap_or_clone(fields)
            }
            Err(fields) => (**fields).as_ref().clone(),
        }
    }
}

impl AsRef<Document> for RetainedDocumentFields {
    fn as_ref(&self) -> &Document {
        &self.0
    }
}

impl std::ops::Deref for RetainedDocumentFields {
    type Target = Document;

    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}

#[cfg(test)]
mod tests;
