//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Incremental live payload sizes keep shared snapshot admission independent of corpus size.

use std::collections::{btree_map::Entry, BTreeMap};
use std::sync::{Arc, Weak};

use parking_lot::Mutex;
use uqa_core::memory::{MemoryError, MemoryReservation};

use super::{DocId, FieldName, IndexedFieldMetadata, MemoryIndexState, MemoryPosting, PostingKey};
use crate::{read_control::StorageReadControl, StorageBackendResult};

#[derive(Debug, Default)]
pub(super) struct RetainedPayload {
    // These sizes describe existing allocations, not requested capacities. A wide sum lets writes preserve their existing error boundary; admission checks the target-sized allowance.
    bytes: u128,
    cached: Mutex<Weak<MemoryReservation>>,
}

impl RetainedPayload {
    fn add(&mut self, bytes: u128) {
        self.bytes += bytes;
        *self.cached.get_mut() = Weak::new();
    }

    fn remove(&mut self, bytes: u128) {
        self.bytes = self
            .bytes
            .checked_sub(bytes)
            .expect("removed payload belongs to the tracked corpus");
        *self.cached.get_mut() = Weak::new();
    }

    pub(super) fn with_retained<T>(
        &self,
        control: &StorageReadControl,
        build: impl FnOnce(Arc<MemoryReservation>) -> StorageBackendResult<T>,
    ) -> StorageBackendResult<T> {
        control.check()?;
        let mut cached = self.cached.lock();
        control.check()?;
        let memory = if let Some(memory) = cached
            .upgrade()
            .filter(|memory| memory.budget().shares_allowance(control.memory()))
        {
            memory
        } else {
            let bytes = usize::try_from(
                self.bytes
                    + size_of::<MemoryIndexState>() as u128
                    + size_of::<MemoryReservation>() as u128,
            )
            .map_err(|_| MemoryError::SizeOverflow)?;
            Arc::new(control.memory().reserve(bytes)?)
        };
        let result = build(Arc::clone(&memory))?;
        control.check()?;
        // Publish only after reader construction succeeds; a late quota or cancellation failure keeps the prior cached allowance.
        *cached = Arc::downgrade(&memory);
        Ok(result)
    }
}

pub(super) fn posting_buffers(posting: &MemoryPosting) -> u128 {
    posting.occurrences.capacity() as u128 * size_of::<uqa_core::TokenOccurrence>() as u128
        + posting.projection.payload.positions.capacity() as u128 * size_of::<u32>() as u128
}

fn posting_size(posting: &MemoryPosting) -> u128 {
    size_of::<(DocId, MemoryPosting)>() as u128 + posting_buffers(posting)
}

fn term_payload(key: &PostingKey) -> u128 {
    key.0.capacity() as u128 + key.1.allocated_bytes() as u128
}

fn index_key_size(key: &PostingKey) -> u128 {
    size_of::<(PostingKey, BTreeMap<DocId, MemoryPosting>)>() as u128 + term_payload(key)
}

fn terms_size(terms: &std::collections::BTreeSet<PostingKey>) -> u128 {
    size_of::<(DocId, std::collections::BTreeSet<PostingKey>)>() as u128
        + terms
            .iter()
            .map(|key| size_of::<PostingKey>() as u128 + term_payload(key))
            .sum::<u128>()
}

fn fields_size(fields: &BTreeMap<FieldName, IndexedFieldMetadata>) -> u128 {
    size_of::<(DocId, BTreeMap<FieldName, IndexedFieldMetadata>)>() as u128
        + fields
            .keys()
            .map(|field| {
                size_of::<(FieldName, IndexedFieldMetadata)>() as u128 + field.capacity() as u128
            })
            .sum::<u128>()
}

fn counter_size(capacity: usize) -> u128 {
    size_of::<(FieldName, u64)>() as u128 + capacity as u128
}

pub(super) fn set_counter(
    target: &mut BTreeMap<FieldName, u64>,
    field: FieldName,
    value: Option<u64>,
    retention: &mut RetainedPayload,
) {
    if let Some(value) = value {
        match target.entry(field) {
            Entry::Vacant(entry) => {
                retention.add(counter_size(entry.key().capacity()));
                entry.insert(value);
            }
            Entry::Occupied(mut entry) => {
                entry.insert(value);
            }
        }
    } else if let Some((field, _)) = target.remove_entry(&field) {
        retention.remove(counter_size(field.capacity()));
    }
}

impl MemoryIndexState {
    pub(super) fn insert_posting(
        &mut self,
        doc_id: DocId,
        key: PostingKey,
        posting: MemoryPosting,
    ) {
        let postings = match self.index.entry(key) {
            Entry::Vacant(entry) => {
                self.retention.add(index_key_size(entry.key()));
                entry.insert(BTreeMap::new())
            }
            Entry::Occupied(entry) => entry.into_mut(),
        };
        self.retention.add(posting_size(&posting));
        if let Some(previous) = postings.insert(doc_id, posting) {
            self.retention.remove(posting_size(&previous));
        }
    }

    pub(super) fn remove_posting(
        &mut self,
        doc_id: DocId,
        key: &PostingKey,
    ) -> StorageBackendResult<()> {
        let postings = self.index.get_mut(key).ok_or_else(|| {
            super::StorageBackendError::Other(format!(
                "inverted-index document {doc_id} lost a validated posting before removal"
            ))
        })?;
        if let Some(posting) = postings.remove(&doc_id) {
            self.retention.remove(posting_size(&posting));
        }
        if postings.is_empty() {
            let (key, _) = self.index.remove_entry(key).expect("validated posting key");
            self.retention.remove(index_key_size(&key));
        }
        Ok(())
    }

    pub(super) fn remove_document_metadata(&mut self, doc_id: DocId) {
        if let Some(fields) = self.doc_fields.remove(&doc_id) {
            self.retention.remove(fields_size(&fields));
        }
        drop(self.take_document_terms(doc_id));
    }

    pub(super) fn take_document_terms(
        &mut self,
        doc_id: DocId,
    ) -> Option<std::collections::BTreeSet<PostingKey>> {
        let terms = self.doc_terms.remove(&doc_id);
        if let Some(terms) = &terms {
            self.retention.remove(terms_size(terms));
        }
        terms
    }

    pub(super) fn insert_postings(
        &mut self,
        key: PostingKey,
        postings: BTreeMap<DocId, MemoryPosting>,
    ) {
        match self.index.entry(key) {
            Entry::Vacant(entry) => {
                self.retention.add(
                    index_key_size(entry.key()) + postings.values().map(posting_size).sum::<u128>(),
                );
                entry.insert(postings);
            }
            Entry::Occupied(mut entry) => {
                for (id, posting) in postings {
                    self.retention.add(posting_size(&posting));
                    if let Some(previous) = entry.get_mut().insert(id, posting) {
                        self.retention.remove(posting_size(&previous));
                    }
                }
            }
        }
    }

    pub(super) fn insert_document_metadata(
        &mut self,
        doc_id: DocId,
        fields: BTreeMap<FieldName, IndexedFieldMetadata>,
        terms: std::collections::BTreeSet<PostingKey>,
    ) {
        self.remove_document_metadata(doc_id);
        self.retention
            .add(fields_size(&fields) + terms_size(&terms));
        self.doc_fields.insert(doc_id, fields);
        self.doc_terms.insert(doc_id, terms);
    }

    fn payload_size(&self) -> u128 {
        self.index
            .iter()
            .map(|(key, postings)| {
                index_key_size(key) + postings.values().map(posting_size).sum::<u128>()
            })
            .sum::<u128>()
            + self.doc_terms.values().map(terms_size).sum::<u128>()
            + self.doc_fields.values().map(fields_size).sum::<u128>()
            + [&self.total_length, &self.field_doc_counts]
                .into_iter()
                .flat_map(BTreeMap::keys)
                .map(|field| counter_size(field.capacity()))
                .sum::<u128>()
    }
}

impl Clone for MemoryIndexState {
    fn clone(&self) -> Self {
        let mut state = Self {
            index: self.index.clone(),
            doc_terms: self.doc_terms.clone(),
            doc_fields: self.doc_fields.clone(),
            total_length: self.total_length.clone(),
            field_doc_counts: self.field_doc_counts.clone(),
            doc_count: self.doc_count,
            retention: RetainedPayload::default(),
        };
        // Cloned strings/vectors may have smaller capacities. Account the new allocations during this existing deep-copy boundary, never during shared capture.
        state.retention.bytes = state.payload_size();
        state
    }
}
