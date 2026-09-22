//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable decoded fields keep their payload reservation through every retained reader.

use super::{Arc, Document, DocumentMetadata, StoredDocument};
use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};
use uqa_core::{
    memory::{Budgeted, MemoryError},
    FieldName, Value, ValueRetentionError,
};

/// One adopted field map and its shared payload charge. Caller-owned input and read outputs retain their own allocation responsibility; clones of this handle retain one lease. Opaque B-tree node slack and allocator bookkeeping are outside the decoded payload allowance.
#[derive(Clone, Debug)]
pub struct RetainedDocumentFields(Arc<Budgeted<Arc<Document>>>);

impl RetainedDocumentFields {
    /// Move already charged decoded fields into shared immutable storage. The input reservation covers live entries, key capacities and value payloads, excluding the map's inline layout. Foreign or incomplete leases are rejected; shared wrappers are reserved before allocation.
    pub fn from_budgeted(
        fields: Budgeted<Document>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let (fields, mut memory) = fields.into_parts();
        if !memory.budget().shares_allowance(control.memory()) {
            drop(fields);
            return Err(StorageBackendError::Other(
                "decoded document reservation belongs to a different allowance".into(),
            ));
        }
        let value = Value::Map(fields);
        let required = value
            .retained_payload_bytes(control.memory(), control.cancellation())
            .map_err(retention_error)?;
        if memory.bytes() < required {
            return Err(StorageBackendError::Other(
                "decoded document reservation does not cover its payload".into(),
            ));
        }
        // Removed migration fields and other discarded payloads need no continuing reservation.
        let surplus = memory.bytes() - required;
        drop(memory.split(surplus));
        memory.grow(size_of::<Document>())?;
        let Value::Map(fields) = value else {
            unreachable!("document field map");
        };
        let fields = Arc::new(fields);
        control.check()?;
        Ok(Self(Budgeted::new(fields, memory).into_shared()?))
    }

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
                .map_err(retention_error)?;
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

pub(super) fn retention_error(error: ValueRetentionError) -> StorageBackendError {
    match error {
        ValueRetentionError::Memory(error) => StorageBackendError::Memory(error),
        ValueRetentionError::Cancelled(error) => StorageBackendError::Cancelled(error),
    }
}

/// An immutable decoded tuple keeps its public fields charged through the last shared reader; tuple metadata remains outside the public field namespace.
#[derive(Clone, Debug)]
pub struct RetainedStoredDocument {
    fields: RetainedDocumentFields,
    metadata: DocumentMetadata,
}

impl RetainedStoredDocument {
    pub fn with_metadata(fields: RetainedDocumentFields, metadata: DocumentMetadata) -> Self {
        Self { fields, metadata }
    }

    pub fn fields(&self) -> &Document {
        &self.fields
    }

    pub fn retained_fields(&self) -> &RetainedDocumentFields {
        &self.fields
    }

    pub fn metadata(&self) -> DocumentMetadata {
        self.metadata
    }

    pub fn into_parts(self) -> (RetainedDocumentFields, DocumentMetadata) {
        (self.fields, self.metadata)
    }

    /// Produce a caller-owned mutable tuple, ending this reader's retained ownership. Unique fields move without copying; retained siblings keep their original payload and lease.
    pub fn into_stored(self) -> StoredDocument {
        StoredDocument::with_metadata(self.fields.into_document(), self.metadata)
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
