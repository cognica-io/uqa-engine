//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Transfer only allocations that survive document publication; staging maps and duplicate keys are released.

use super::{
    BTreeMap, DocId, FieldName, IndexedFieldMetadata, MemoryIndexState, MemoryReplacementPlan,
    StagedMemoryDocument, StorageBackendResult, StorageReadControl,
};
use crate::inverted_index::{MemoryPosting, PostingKey};
use std::collections::BTreeSet;
use uqa_core::{memory::MemoryError, TokenOccurrence};

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

    fn entries<T>(&mut self, count: usize) -> StorageBackendResult<()> {
        let bytes = count
            .checked_mul(size_of::<T>())
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
    charge.entries::<(DocId, BTreeMap<FieldName, IndexedFieldMetadata>)>(1)?;
    charge.entries::<(DocId, BTreeSet<PostingKey>)>(1)?;
    for field in staged.fields.keys() {
        control.check()?;
        charge.keep(size_of::<(FieldName, IndexedFieldMetadata)>())?;
        charge.keep(field.capacity())?;
    }
    for (field, term) in &staged.terms {
        control.check()?;
        charge.keep(size_of::<PostingKey>())?;
        charge.keep(field.capacity())?;
        charge.keep(term.allocated_bytes())?;
    }
    for (key, posting) in &staged.postings {
        control.check()?;
        charge.entries::<(DocId, MemoryPosting)>(1)?;
        charge.keep(
            posting
                .occurrences
                .capacity()
                .checked_mul(size_of::<TokenOccurrence>())
                .ok_or(MemoryError::SizeOverflow)?,
        )?;
        charge.keep(
            posting
                .projection
                .payload
                .positions
                .capacity()
                .checked_mul(size_of::<u32>())
                .ok_or(MemoryError::SizeOverflow)?,
        )?;
        if !state.index.contains_key(key) {
            charge.entries::<(PostingKey, BTreeMap<DocId, MemoryPosting>)>(1)?;
            charge.keep(key.0.capacity())?;
            charge.keep(key.1.allocated_bytes())?;
        }
    }
    for (field, counters) in &plan.field_counters {
        control.check()?;
        if !state.total_length.contains_key(field) {
            charge.entries::<(FieldName, u64)>(1)?;
            charge.keep(counters.total_key.capacity())?;
        }
        if !state.field_doc_counts.contains_key(field) {
            charge.entries::<(FieldName, u64)>(1)?;
            charge.keep(field.capacity())?;
        }
    }
    Ok(charge)
}
