//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Transfer admitted retained nodes and payloads; projection/counter scratch and duplicate keys are released.

use super::{
    DocId, FieldName, IndexedFieldMetadata, MemoryIndexState, MemoryReplacementPlan,
    StagedMemoryDocument, StorageBackendResult, StorageReadControl,
};
use crate::inverted_index::{MemoryPosting, PostingKey};
use uqa_core::memory::{MemoryError, OwnedMap, OwnedSet};

pub(super) struct Charge {
    pub retained: usize,
    pub new_entries: usize,
}

impl Charge {
    fn keep(&mut self, bytes: usize) -> StorageBackendResult<()> {
        self.retained = self
            .retained
            .checked_add(bytes)
            .ok_or(MemoryError::SizeOverflow)?;
        Ok(())
    }

    fn entries<K, V>(&mut self, count: usize) -> StorageBackendResult<()> {
        let bytes = count
            .checked_mul(OwnedMap::<K, V>::entry_bytes())
            .ok_or(MemoryError::SizeOverflow)?;
        self.new_entries = self
            .new_entries
            .checked_add(bytes)
            .ok_or(MemoryError::SizeOverflow)?;
        self.keep(bytes)
    }
}

pub(super) fn new_document(
    state: &MemoryIndexState,
    staged: &StagedMemoryDocument,
    plan: &MemoryReplacementPlan,
    control: &StorageReadControl,
) -> StorageBackendResult<Charge> {
    let mut charge = Charge {
        retained: 0,
        new_entries: 0,
    };
    charge.entries::<DocId, OwnedMap<FieldName, IndexedFieldMetadata>>(1)?;
    charge.entries::<DocId, OwnedSet<PostingKey>>(1)?;
    for field in staged.fields.keys() {
        control.check()?;
        charge.keep(OwnedMap::<FieldName, IndexedFieldMetadata>::entry_bytes())?;
        charge.keep(field.capacity())?;
    }
    for (field, term) in &staged.terms {
        control.check()?;
        charge.keep(OwnedSet::<PostingKey>::entry_bytes())?;
        charge.keep(field.capacity())?;
        charge.keep(term.allocated_bytes())?;
    }
    for (key, posting) in &staged.postings {
        control.check()?;
        charge.entries::<DocId, MemoryPosting>(1)?;
        charge.keep(
            usize::try_from(super::super::footprint::posting_buffers(posting))
                .map_err(|_| MemoryError::SizeOverflow)?,
        )?;
        if !state.index.contains_key(key) {
            charge.entries::<PostingKey, OwnedMap<DocId, MemoryPosting>>(1)?;
            charge.keep(key.0.capacity())?;
            charge.keep(key.1.allocated_bytes())?;
        }
    }
    for (field, counters) in &plan.field_counters {
        control.check()?;
        if !state.total_length.contains_key(field) {
            charge.entries::<FieldName, u64>(1)?;
            charge.keep(counters.total_key.capacity())?;
        }
        if !state.field_doc_counts.contains_key(field) {
            charge.entries::<FieldName, u64>(1)?;
            charge.keep(field.capacity())?;
        }
    }
    Ok(charge)
}
