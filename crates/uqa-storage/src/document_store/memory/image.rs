//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Decoded tuples enter writable native rows without losing their original admission.

use std::sync::Arc;

use uqa_core::{
    memory::{Budgeted, BudgetedVec, MemoryError, MemoryReservation, OwnedMap},
    DocId, Value,
};

use super::{
    DocumentLayouts, DocumentRows, MemoryDocumentRow, MemoryDocumentState, MemoryDocumentStore,
    ProjectionCache, SharedDocumentValues, StoredDocument,
};
use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};

/// An unpublished writable document corpus. Adding a tuple consumes this builder, so rejection, cancellation and unwinding retire the whole partial corpus. Completed rows retain the decoding allowance independently of the builder and its cancellation signal.
pub struct MemoryDocumentStoreBuilder {
    rows: OwnedMap<DocId, MemoryDocumentRow>,
    layouts: DocumentLayouts,
    row_payload_bytes: u128,
    control: StorageReadControl,
}

impl MemoryDocumentStoreBuilder {
    pub fn new(control: &StorageReadControl) -> Self {
        Self {
            rows: OwnedMap::new(),
            layouts: DocumentLayouts::for_images(control),
            row_payload_bytes: 0,
            control: control.clone(),
        }
    }

    /// Adopt one complete, uniquely identified tuple under this builder's allowance. Values and field names move into their native shared owners; no SQL or analyzer is executed.
    pub fn add_document(
        mut self,
        id: DocId,
        document: Budgeted<StoredDocument>,
    ) -> StorageBackendResult<Self> {
        self.control.check()?;
        if self.rows.contains_key(&id) {
            return Err(invalid("native document image repeats a document identity"));
        }
        let (values, names, metadata, value_bytes) = convert(document, &self.control)?;
        let layout_id = self.layouts.store_image_fields(names, &self.control)?;
        let values = values.into_shared()?;
        let payload_bytes = value_bytes as u128
            + values.capacity() as u128 * size_of::<Value>() as u128
            + size_of::<Budgeted<Vec<Value>>>() as u128;
        self.control.check()?;
        self.rows.insert_budgeted(
            id,
            MemoryDocumentRow {
                layout_id,
                values: SharedDocumentValues::Retained(values),
                metadata,
                payload_bytes,
            },
            self.control.memory(),
        )?;
        self.row_payload_bytes += payload_bytes;
        self.control.check()?;
        Ok(self)
    }

    pub fn finish(self) -> StorageBackendResult<MemoryDocumentStore> {
        self.control.check()?;
        let memory = self
            .control
            .memory()
            .reserve(size_of::<MemoryDocumentState>() + size_of::<MemoryDocumentStore>())?;
        let state = MemoryDocumentState {
            documents: DocumentRows::Owned(self.rows),
            layouts: self.layouts,
            row_payload_bytes: self.row_payload_bytes,
            retention: crate::retained_memory::RetainedMemory::default(),
            _image_memory: Some(memory),
        };
        self.control.check()?;
        Ok(MemoryDocumentStore {
            state: Arc::new(state),
            projections: ProjectionCache::default(),
            retention: None,
        })
    }
}

struct Conversion {
    source: StoredDocument,
    values: BudgetedVec<Value>,
    names: BudgetedVec<String>,
    payload: MemoryReservation,
}

type Converted = (
    Budgeted<Vec<Value>>,
    Budgeted<Vec<String>>,
    super::DocumentMetadata,
    usize,
);

fn convert(
    document: Budgeted<StoredDocument>,
    control: &StorageReadControl,
) -> StorageBackendResult<Converted> {
    if !document.budget().shares_allowance(control.memory()) {
        return Err(invalid(
            "native document input belongs to a different allowance",
        ));
    }
    let mut names_bytes = 0_usize;
    let mut value_bytes = 0_usize;
    for (name, value) in document.fields() {
        control.check()?;
        names_bytes = names_bytes
            .checked_add(name.capacity())
            .ok_or(MemoryError::SizeOverflow)?;
        value_bytes = value_bytes
            .checked_add(
                value
                    .retained_payload_bytes(control.memory(), control.cancellation())
                    .map_err(super::super::retained::retention_error)?,
            )
            .ok_or(MemoryError::SizeOverflow)?;
    }
    let entries = document
        .fields()
        .len()
        .checked_mul(size_of::<(String, Value)>())
        .ok_or(MemoryError::SizeOverflow)?;
    let required = entries
        .checked_add(names_bytes)
        .and_then(|bytes| bytes.checked_add(value_bytes))
        .ok_or(MemoryError::SizeOverflow)?;
    if document.reserved_bytes() < required {
        return Err(invalid(
            "native document input reservation does not cover its payload",
        ));
    }
    let metadata = document.metadata();
    let (source, payload) = document.into_parts();
    let mut converted = Conversion {
        source,
        values: BudgetedVec::new(control.memory()),
        names: BudgetedVec::new(control.memory()),
        payload,
    };
    let count = converted.source.fields().len();
    converted.values.reserve(count)?;
    converted.names.reserve(count)?;
    let fields = std::mem::take(converted.source.fields_mut());
    for (name, value) in fields {
        control.check()?;
        converted.names.push(name)?;
        converted.values.push(value)?;
    }
    control.check()?;
    let Conversion {
        source,
        values,
        names,
        mut payload,
    } = converted;
    drop(source);
    let (values, mut values_memory) = values.into_parts();
    values_memory.absorb(payload.split(value_bytes));
    let values = Budgeted::new(values, values_memory);
    let (names, mut names_memory) = names.into_parts();
    names_memory.absorb(payload.split(names_bytes));
    let names = Budgeted::new(names, names_memory);
    drop(payload);
    Ok((values, names, metadata, value_bytes))
}

fn invalid(message: &'static str) -> StorageBackendError {
    StorageBackendError::Other(message.into())
}

#[cfg(test)]
mod tests;
