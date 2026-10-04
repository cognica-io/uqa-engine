//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A source rebuild of a text index staged before it is written. The field records of its documents and its postings are private records ordered as the index stores them, which spill to encrypted temporary files once the allowance is under pressure, as a transaction's own changes do. Writing the index then needs memory for one cluster at a time instead of for every posting.

use std::collections::BTreeMap;

use uqa_core::memory::BudgetedVec;
use uqa_core::{DocId, FieldName, TokenOccurrence};

use super::{IndexedFieldMetadata, IndexedFieldRevision};
use crate::backend::{StorageBackendError, StorageBackendResult};
use crate::clustered_postings::{cluster_id, encode_posting, OccurrenceClusterBuilder};
use crate::mvcc::{PrivateRecordChanges, RecordWrite, VersionError};
use crate::read_control::StorageReadControl;
use crate::TokenTermKey;

/// The records of documents' fields, keyed by document and field.
const DOCUMENT: u8 = 0;
/// The records of postings, keyed by field, term and document.
const POSTING: u8 = 1;
/// The records a page of a visit keeps at most.
const PAGE_RECORDS: usize = 1024;
/// The bytes after which a page of a visit ends: a sixteenth of the allowance, within these bounds.
const PAGE_BYTES: std::ops::RangeInclusive<usize> = (64 << 10)..=(1 << 20);

/// The indexed documents of one field and their total length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldTotals {
    pub revision: IndexedFieldRevision,
    pub doc_count: u64,
    pub total_length: u64,
}

/// The analyzed fields of a document a rebuild stages: each field's name, metadata and the occurrences of each of its terms.
pub type StagedFields<'a> = (
    &'a str,
    &'a IndexedFieldMetadata,
    &'a BTreeMap<TokenTermKey, Vec<TokenOccurrence>>,
);

/// A staged field of a document: its metadata and the encoded keys of its terms.
pub struct StagedFieldRecord<'a> {
    pub doc_id: DocId,
    pub field: &'a str,
    pub metadata: IndexedFieldMetadata,
    pub terms: &'a [u8],
}

/// A staged cluster with its encoded score and positions values.
pub struct StagedCluster<'a> {
    pub field: &'a str,
    pub term: &'a TokenTermKey,
    pub cluster: u64,
    pub score: &'a [u8],
    pub positions: &'a [u8],
}

/// A visit of staged records, each a key without its kind and a value.
type RecordVisit<'a> = &'a mut dyn FnMut(&[u8], &[u8]) -> StorageBackendResult<()>;

/// A source rebuild of a text index staged before it is written: documents are staged in ascending identity order, then visited by field record and by cluster in the order the index stores them.
pub struct SourceRebuild {
    records: PrivateRecordChanges,
    totals: BTreeMap<FieldName, FieldTotals>,
    last: Option<DocId>,
    control: StorageReadControl,
}

fn invalid(message: &str) -> StorageBackendError {
    StorageBackendError::Other(message.into())
}

/// Append `bytes` so that encoded keys order as their byte strings do: a zero byte is followed by 0xff, and the component ends with a zero byte followed by one.
fn push_component(key: &mut BudgetedVec<u8>, bytes: &[u8]) -> StorageBackendResult<()> {
    key.reserve(bytes.len().saturating_add(2))?;
    for &byte in bytes {
        key.push(byte)?;
        if byte == 0 {
            key.push(0xff)?;
        }
    }
    key.extend_from_slice(&[0, 1])?;
    Ok(())
}

/// Read a component `push_component` appended, returning it and the rest of the key.
fn read_component(key: &[u8]) -> StorageBackendResult<(Vec<u8>, &[u8])> {
    let mut bytes = Vec::new();
    let mut index = 0;
    while index < key.len() {
        match (key[index], key.get(index + 1)) {
            (0, Some(0xff)) => bytes.push(0),
            (0, Some(1)) => return Ok((bytes, &key[index + 2..])),
            (0, _) => return Err(invalid("a staged rebuild key component is malformed")),
            (byte, _) => {
                bytes.push(byte);
                index += 1;
                continue;
            }
        }
        index += 2;
    }
    Err(invalid("a staged rebuild key component is unterminated"))
}

/// Count a staged field of a document in `totals_by_field`.
fn count(
    totals_by_field: &mut BTreeMap<FieldName, FieldTotals>,
    field: &str,
    metadata: &IndexedFieldMetadata,
) -> StorageBackendResult<()> {
    let revision = metadata.revision();
    if !totals_by_field.contains_key(field) {
        totals_by_field.insert(
            field.to_owned(),
            FieldTotals {
                revision,
                doc_count: 0,
                total_length: 0,
            },
        );
    }
    let totals = totals_by_field.get_mut(field).expect("a counted field");
    if totals.revision != revision {
        return Err(invalid(
            "indexed field revisions disagree during replacement",
        ));
    }
    totals.doc_count = totals
        .doc_count
        .checked_add(1)
        .ok_or_else(|| invalid("field document count overflow"))?;
    totals.total_length = totals
        .total_length
        .checked_add(metadata.length)
        .ok_or_else(|| invalid("total field length overflow"))?;
    Ok(())
}

fn read_doc_id(bytes: &[u8]) -> StorageBackendResult<DocId> {
    bytes
        .try_into()
        .map(DocId::from_be_bytes)
        .map_err(|_| invalid("a staged rebuild key identity is malformed"))
}

impl SourceRebuild {
    pub fn new(control: &StorageReadControl) -> Self {
        Self {
            records: PrivateRecordChanges::new(control.memory()),
            totals: BTreeMap::new(),
            last: None,
            control: control.clone(),
        }
    }

    /// Stage document `doc_id` with its analyzed fields. Documents are staged in strictly ascending identity order.
    pub fn stage<'a>(
        &mut self,
        doc_id: DocId,
        fields: impl IntoIterator<Item = StagedFields<'a>>,
    ) -> StorageBackendResult<()> {
        self.control.check()?;
        if self.last.is_some_and(|last| last >= doc_id) {
            return Err(invalid(
                "a source rebuild reads documents in strictly ascending identity order",
            ));
        }
        self.last = Some(doc_id);
        let memory = self.control.memory();
        let mut records = BudgetedVec::<(BudgetedVec<u8>, BudgetedVec<u8>)>::new(memory);
        for (field, metadata, terms) in fields {
            count(&mut self.totals, field, metadata)?;
            let mut key = BudgetedVec::new(memory);
            key.push(DOCUMENT)?;
            key.extend_from_slice(&doc_id.to_be_bytes())?;
            push_component(&mut key, field.as_bytes())?;
            let mut value = BudgetedVec::new(memory);
            let encoded = metadata.to_bytes()?;
            value.push(
                u8::try_from(encoded.len())
                    .map_err(|_| invalid("indexed field metadata exceeds a staged record"))?,
            )?;
            value.extend_from_slice(&encoded)?;
            value.extend_from_slice(&crate::clustered_postings::encode_term_key_refs(
                terms.keys(),
            )?)?;
            records.push((key, value))?;
            for (term, occurrences) in terms {
                self.control.check()?;
                let mut key = BudgetedVec::new(memory);
                key.push(POSTING)?;
                push_component(&mut key, field.as_bytes())?;
                push_component(&mut key, term.as_bytes())?;
                key.extend_from_slice(&doc_id.to_be_bytes())?;
                let mut value = BudgetedVec::new(memory);
                encode_posting(metadata.length, occurrences, &mut value, &self.control)?;
                records.push((key, value))?;
            }
        }
        let mut writes = BudgetedVec::new(memory);
        writes.reserve(records.len())?;
        for (key, value) in records.iter() {
            writes.push(RecordWrite {
                key,
                expected: None,
                value: Some(value),
            })?;
        }
        self.records
            .apply(&writes, &self.control)
            .map_err(VersionError::into_storage_error)
    }

    /// The indexed documents and total length of each staged field.
    pub fn totals(&self) -> &BTreeMap<FieldName, FieldTotals> {
        &self.totals
    }

    /// Visit the staged field records in order of document and field, each with its metadata and the encoded keys of its terms.
    pub fn visit_documents(
        &self,
        visit: &mut dyn FnMut(StagedFieldRecord<'_>) -> StorageBackendResult<()>,
    ) -> StorageBackendResult<()> {
        self.visit_records(DOCUMENT, &mut |key, value| {
            let (id, field) = key
                .split_at_checked(size_of::<DocId>())
                .ok_or_else(|| invalid("a staged field record key is malformed"))?;
            let (field, trailing) = read_component(field)?;
            let (metadata, terms) = value
                .split_first()
                .and_then(|(&length, encoded)| encoded.split_at_checked(usize::from(length)))
                .filter(|_| trailing.is_empty())
                .ok_or_else(|| invalid("a staged field record is malformed"))?;
            let field = String::from_utf8(field)
                .map_err(|_| invalid("a staged field name is not UTF-8"))?;
            visit(StagedFieldRecord {
                doc_id: read_doc_id(id)?,
                field: &field,
                metadata: IndexedFieldMetadata::from_bytes(metadata)?,
                terms,
            })
        })
    }

    /// Visit the staged postings as clusters in order of field, term and cluster, each with its encoded score and positions values.
    pub fn visit_clusters(
        &self,
        visit: &mut dyn FnMut(StagedCluster<'_>) -> StorageBackendResult<()>,
    ) -> StorageBackendResult<()> {
        let mut current: Option<(String, TokenTermKey, u64, OccurrenceClusterBuilder)> = None;
        let mut finish = |cluster: Option<(
            String,
            TokenTermKey,
            u64,
            OccurrenceClusterBuilder,
        )>| {
            match cluster {
                Some((field, term, cluster, builder)) => {
                    let (score, positions) = builder.finish(&self.control)?;
                    visit(StagedCluster {
                        field: &field,
                        term: &term,
                        cluster,
                        score: &score,
                        positions: &positions,
                    })
                }
                None => Ok(()),
            }
        };
        self.visit_records(POSTING, &mut |key, value| {
            let (field, rest) = read_component(key)?;
            let (term, id) = read_component(rest)?;
            let doc_id = read_doc_id(id)?;
            let cluster = cluster_id(doc_id);
            let same = current
                .as_ref()
                .is_some_and(|(current_field, current_term, id, _)| {
                    *id == cluster
                        && current_field.as_bytes() == field.as_slice()
                        && current_term.as_bytes() == term.as_slice()
                });
            if !same {
                finish(current.take())?;
                let field = String::from_utf8(field)
                    .map_err(|_| invalid("a staged field name is not UTF-8"))?;
                current = Some((
                    field,
                    TokenTermKey::from_bytes(term)?,
                    cluster,
                    OccurrenceClusterBuilder::new(&self.control)?,
                ));
            }
            let (_, _, _, builder) = current.as_mut().expect("a current cluster");
            builder.push_encoded(doc_id, value)
        })?;
        finish(current.take())
    }

    /// Visit the staged records of `kind` in key order, without its kind byte, a bounded page at a time so that no visit runs while the records are read.
    fn visit_records(&self, kind: u8, visit: RecordVisit<'_>) -> StorageBackendResult<()> {
        let snapshot = self
            .records
            .snapshot()
            .map_err(VersionError::into_storage_error)?;
        let memory = self.control.memory();
        let page_bytes = (memory.limit() / 16).clamp(*PAGE_BYTES.start(), *PAGE_BYTES.end());
        let mut after: Option<BudgetedVec<u8>> = None;
        loop {
            let mut page = BudgetedVec::<(BudgetedVec<u8>, BudgetedVec<u8>)>::new(memory);
            let mut bytes = 0_usize;
            let mut full = false;
            snapshot
                .visit(&[kind], after.as_deref(), &self.control, &mut |write| {
                    let mut key = BudgetedVec::new(memory);
                    key.extend_from_slice(write.key())?;
                    let mut value = BudgetedVec::new(memory);
                    value.extend_from_slice(write.value().ok_or_else(|| {
                        VersionError::from(invalid("a staged rebuild record is deleted"))
                    })?)?;
                    bytes = bytes.saturating_add(key.len() + value.len());
                    page.push((key, value))?;
                    full = page.len() == PAGE_RECORDS || bytes >= page_bytes;
                    Ok(!full)
                })
                .map_err(VersionError::into_storage_error)?;
            for (key, value) in page.iter() {
                self.control.check()?;
                visit(&key[1..], value)?;
            }
            let Some((last, _)) = page.last().filter(|_| full) else {
                return Ok(());
            };
            let mut next = BudgetedVec::new(memory);
            next.extend_from_slice(last)?;
            after = Some(next);
        }
    }
}

#[cfg(test)]
mod tests;
