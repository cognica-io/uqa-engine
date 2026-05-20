//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Backend-neutral Key/Value storage.
//!
//! This module is the logical storage boundary for non-relational
//! persistence. Concrete stores only need ordered byte keys, atomic
//! batches, prefix scans, and transaction hooks. Catalog, document,
//! inverted-index, and vector-index behavior stays above that boundary.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uqa_analysis::Analyzer;
use uqa_core::{DocId, FieldName, IndexStats, Payload, PostingEntry, PostingList, Value};

use crate::backend::{PersistentStorageBackend, PersistentVectorIndexParams};
use crate::catalog::{
    CatalogFacade, CatalogIndexRow, ColumnStatsInput, ColumnStatsRow, EdgeRow, ForeignTableRow,
    TableSchema,
};
use crate::document_store::{Document, DocumentStore};
use crate::inverted_index::{AnalyzerPhase, InvertedIndex};
use crate::vector_index::{cosine_similarity, VectorIndex};
use crate::{StorageBackendError, StorageBackendResult};

const TAG_METADATA: u8 = b'm';
const TAG_TABLE: u8 = b't';
const TAG_MODEL: u8 = b'M';
const TAG_SCORING_PARAMS: u8 = b'S';
const TAG_NAMED_GRAPH: u8 = b'g';
const TAG_VERTEX: u8 = b'V';
const TAG_EDGE: u8 = b'E';
const TAG_GRAPH_MEMBERSHIP: u8 = b'G';
const TAG_ANALYZER: u8 = b'a';
const TAG_TABLE_FIELD_ANALYZER: u8 = b'A';
const TAG_FOREIGN_SERVER: u8 = b'F';
const TAG_FOREIGN_TABLE: u8 = b'T';
const TAG_CATALOG_INDEX: u8 = b'C';
const TAG_PATH_INDEX: u8 = b'P';
const TAG_COLUMN_STATS: u8 = b'c';
const TAG_DOCUMENT: u8 = b'd';
const TAG_POSTING: u8 = b'p';
const TAG_DOC_LENGTH: u8 = b'l';
const TAG_FIELD_STATS: u8 = b'f';
const TAG_REVERSE_POSTING: u8 = b'r';
const TAG_VECTOR: u8 = b'v';

#[derive(Debug, Clone)]
enum KeyValueBatchOperation {
    Put(Vec<u8>, Vec<u8>),
    Delete(Vec<u8>),
    DeletePrefix(Vec<u8>),
}

/// Atomic mutation buffer for a [`KeyValueStore`].
pub trait KeyValueBatch {
    fn put(&mut self, key: &[u8], value: &[u8]) -> StorageBackendResult<()>;
    fn delete(&mut self, key: &[u8]) -> StorageBackendResult<()>;
    fn delete_prefix(&mut self, prefix: &[u8]) -> StorageBackendResult<()>;
    fn commit(self: Box<Self>) -> StorageBackendResult<()>;
}

/// Ordered byte-key storage used by Key/Value catalog and index backends.
pub trait KeyValueStore: Send + Sync {
    fn get(&self, key: &[u8]) -> StorageBackendResult<Option<Vec<u8>>>;
    fn put(&self, key: &[u8], value: &[u8]) -> StorageBackendResult<()>;
    fn delete(&self, key: &[u8]) -> StorageBackendResult<()>;
    fn scan_prefix(&self, prefix: &[u8]) -> StorageBackendResult<Vec<(Vec<u8>, Vec<u8>)>>;
    fn delete_prefix(&self, prefix: &[u8]) -> StorageBackendResult<usize>;
    fn batch(&self) -> Box<dyn KeyValueBatch + '_>;

    fn begin_transaction(&self) -> StorageBackendResult<()> {
        Err(StorageBackendError::Other(
            "KeyValue transaction begin is not implemented for this store".into(),
        ))
    }

    fn commit_transaction(&self) -> StorageBackendResult<()> {
        Err(StorageBackendError::Other(
            "KeyValue transaction commit is not implemented for this store".into(),
        ))
    }

    fn rollback_transaction(&self) -> StorageBackendResult<()> {
        Err(StorageBackendError::Other(
            "KeyValue transaction rollback is not implemented for this store".into(),
        ))
    }

    fn savepoint(&self, _name: &str) -> StorageBackendResult<()> {
        Err(StorageBackendError::Other(
            "KeyValue savepoints are not implemented for this store".into(),
        ))
    }

    fn release_savepoint(&self, _name: &str) -> StorageBackendResult<()> {
        Err(StorageBackendError::Other(
            "KeyValue savepoint release is not implemented for this store".into(),
        ))
    }

    fn rollback_to_savepoint(&self, _name: &str) -> StorageBackendResult<()> {
        Err(StorageBackendError::Other(
            "KeyValue savepoint rollback is not implemented for this store".into(),
        ))
    }
}

/// In-memory Key/Value store used by trait-level tests and future non-SQL
/// fixtures.
#[derive(Debug, Default)]
pub struct MemoryKeyValueStore {
    inner: Mutex<MemoryKeyValueState>,
}

#[derive(Debug, Default, Clone)]
struct MemoryKeyValueState {
    map: BTreeMap<Vec<u8>, Vec<u8>>,
    transactions: Vec<BTreeMap<Vec<u8>, Vec<u8>>>,
    savepoints: BTreeMap<String, BTreeMap<Vec<u8>, Vec<u8>>>,
}

impl MemoryKeyValueStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl KeyValueStore for MemoryKeyValueStore {
    fn get(&self, key: &[u8]) -> StorageBackendResult<Option<Vec<u8>>> {
        Ok(self.inner.lock().map.get(key).cloned())
    }

    fn put(&self, key: &[u8], value: &[u8]) -> StorageBackendResult<()> {
        self.inner.lock().map.insert(key.to_vec(), value.to_vec());
        Ok(())
    }

    fn delete(&self, key: &[u8]) -> StorageBackendResult<()> {
        self.inner.lock().map.remove(key);
        Ok(())
    }

    fn scan_prefix(&self, prefix: &[u8]) -> StorageBackendResult<Vec<(Vec<u8>, Vec<u8>)>> {
        Ok(self
            .inner
            .lock()
            .map
            .range(prefix.to_vec()..)
            .take_while(|(key, _)| key.starts_with(prefix))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect())
    }

    fn delete_prefix(&self, prefix: &[u8]) -> StorageBackendResult<usize> {
        let mut inner = self.inner.lock();
        let keys = inner
            .map
            .range(prefix.to_vec()..)
            .take_while(|(key, _)| key.starts_with(prefix))
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in &keys {
            inner.map.remove(key);
        }
        Ok(keys.len())
    }

    fn batch(&self) -> Box<dyn KeyValueBatch + '_> {
        Box::new(MemoryKeyValueBatch {
            store: self,
            operations: Vec::new(),
        })
    }

    fn begin_transaction(&self) -> StorageBackendResult<()> {
        let mut inner = self.inner.lock();
        let snapshot = inner.map.clone();
        inner.transactions.push(snapshot);
        Ok(())
    }

    fn commit_transaction(&self) -> StorageBackendResult<()> {
        let mut inner = self.inner.lock();
        inner.transactions.pop().ok_or_else(|| {
            StorageBackendError::Other("no open KeyValue transaction to commit".into())
        })?;
        inner.savepoints.clear();
        Ok(())
    }

    fn rollback_transaction(&self) -> StorageBackendResult<()> {
        let mut inner = self.inner.lock();
        let snapshot = inner.transactions.pop().ok_or_else(|| {
            StorageBackendError::Other("no open KeyValue transaction to roll back".into())
        })?;
        inner.map = snapshot;
        inner.savepoints.clear();
        Ok(())
    }

    fn savepoint(&self, name: &str) -> StorageBackendResult<()> {
        let mut inner = self.inner.lock();
        let snapshot = inner.map.clone();
        inner.savepoints.insert(name.to_string(), snapshot);
        Ok(())
    }

    fn release_savepoint(&self, name: &str) -> StorageBackendResult<()> {
        self.inner.lock().savepoints.remove(name);
        Ok(())
    }

    fn rollback_to_savepoint(&self, name: &str) -> StorageBackendResult<()> {
        let mut inner = self.inner.lock();
        let snapshot = inner
            .savepoints
            .get(name)
            .cloned()
            .ok_or_else(|| StorageBackendError::Other(format!("unknown savepoint `{name}`")))?;
        inner.map = snapshot;
        Ok(())
    }
}

struct MemoryKeyValueBatch<'a> {
    store: &'a MemoryKeyValueStore,
    operations: Vec<KeyValueBatchOperation>,
}

impl KeyValueBatch for MemoryKeyValueBatch<'_> {
    fn put(&mut self, key: &[u8], value: &[u8]) -> StorageBackendResult<()> {
        self.operations
            .push(KeyValueBatchOperation::Put(key.to_vec(), value.to_vec()));
        Ok(())
    }

    fn delete(&mut self, key: &[u8]) -> StorageBackendResult<()> {
        self.operations
            .push(KeyValueBatchOperation::Delete(key.to_vec()));
        Ok(())
    }

    fn delete_prefix(&mut self, prefix: &[u8]) -> StorageBackendResult<()> {
        self.operations
            .push(KeyValueBatchOperation::DeletePrefix(prefix.to_vec()));
        Ok(())
    }

    fn commit(self: Box<Self>) -> StorageBackendResult<()> {
        let mut inner = self.store.inner.lock();
        for operation in self.operations {
            match operation {
                KeyValueBatchOperation::Put(key, value) => {
                    inner.map.insert(key, value);
                }
                KeyValueBatchOperation::Delete(key) => {
                    inner.map.remove(&key);
                }
                KeyValueBatchOperation::DeletePrefix(prefix) => {
                    let keys = inner
                        .map
                        .range(prefix.clone()..)
                        .take_while(|(key, _)| key.starts_with(&prefix))
                        .map(|(key, _)| key.clone())
                        .collect::<Vec<_>>();
                    for key in keys {
                        inner.map.remove(&key);
                    }
                }
            }
        }
        Ok(())
    }
}

pub fn prefix_upper_bound(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut upper = prefix.to_vec();
    for byte in upper.iter_mut().rev() {
        if *byte != u8::MAX {
            *byte += 1;
            upper.truncate(upper.len());
            return Some(upper);
        }
    }
    None
}

fn other_error(message: impl Into<String>) -> StorageBackendError {
    StorageBackendError::Other(message.into())
}

fn encode_value<T: Serialize>(value: &T) -> StorageBackendResult<Vec<u8>> {
    serde_json::to_vec(value).map_err(StorageBackendError::from)
}

fn decode_value<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> StorageBackendResult<T> {
    serde_json::from_slice(bytes).map_err(StorageBackendError::from)
}

fn string_value(value: &str) -> Vec<u8> {
    value.as_bytes().to_vec()
}

fn decode_string(bytes: Vec<u8>) -> StorageBackendResult<String> {
    String::from_utf8(bytes).map_err(|err| other_error(format!("invalid utf-8 value: {err}")))
}

fn key_with_tag(tag: u8) -> Vec<u8> {
    vec![tag]
}

fn push_segment(key: &mut Vec<u8>, segment: &[u8]) {
    let len: u32 = segment
        .len()
        .try_into()
        .expect("KeyValue segment exceeds u32 length");
    key.extend_from_slice(&len.to_be_bytes());
    key.extend_from_slice(segment);
}

fn push_str(key: &mut Vec<u8>, segment: &str) {
    push_segment(key, segment.as_bytes());
}

fn push_u64(key: &mut Vec<u8>, value: u64) {
    key.extend_from_slice(&value.to_be_bytes());
}

fn read_segment<'a>(key: &'a [u8], offset: &mut usize) -> StorageBackendResult<&'a [u8]> {
    let end = offset.saturating_add(4);
    let len_bytes: [u8; 4] = key
        .get(*offset..end)
        .ok_or_else(|| other_error("truncated KeyValue key segment length"))?
        .try_into()
        .map_err(|_| other_error("invalid KeyValue key segment length"))?;
    *offset = end;
    let len = u32::from_be_bytes(len_bytes) as usize;
    let end = offset.saturating_add(len);
    let segment = key
        .get(*offset..end)
        .ok_or_else(|| other_error("truncated KeyValue key segment"))?;
    *offset = end;
    Ok(segment)
}

fn read_str(key: &[u8], offset: &mut usize) -> StorageBackendResult<String> {
    let segment = read_segment(key, offset)?;
    std::str::from_utf8(segment)
        .map(str::to_string)
        .map_err(|err| other_error(format!("invalid utf-8 key segment: {err}")))
}

fn read_u64(key: &[u8], offset: &mut usize) -> StorageBackendResult<u64> {
    let end = offset.saturating_add(8);
    let bytes: [u8; 8] = key
        .get(*offset..end)
        .ok_or_else(|| other_error("truncated KeyValue u64 key segment"))?
        .try_into()
        .map_err(|_| other_error("invalid KeyValue u64 key segment"))?;
    *offset = end;
    Ok(u64::from_be_bytes(bytes))
}

fn table_prefixed_key(tag: u8, table: &str) -> Vec<u8> {
    let mut key = key_with_tag(tag);
    push_str(&mut key, table);
    key
}

fn single_str_key(tag: u8, name: &str) -> Vec<u8> {
    let mut key = key_with_tag(tag);
    push_str(&mut key, name);
    key
}

fn u64_value(value: u64) -> Vec<u8> {
    value.to_be_bytes().to_vec()
}

fn decode_u64_value(bytes: &[u8]) -> StorageBackendResult<u64> {
    let array: [u8; 8] = bytes
        .try_into()
        .map_err(|_| other_error("invalid u64 KeyValue payload"))?;
    Ok(u64::from_be_bytes(array))
}

fn positions_to_blob(positions: &[u32]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(positions.len() * 4);
    for p in positions {
        buf.extend_from_slice(&p.to_le_bytes());
    }
    buf
}

fn blob_to_positions(blob: &[u8]) -> Vec<u32> {
    blob.chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn vector_to_blob(v: &[f32]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(v.len() * 4);
    for x in v {
        buf.extend_from_slice(&x.to_le_bytes());
    }
    buf
}

fn blob_to_vector(blob: &[u8]) -> Vec<f32> {
    blob.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn document_key_prefix(table: &str) -> Vec<u8> {
    table_prefixed_key(TAG_DOCUMENT, table)
}

fn document_key(table: &str, doc_id: DocId) -> Vec<u8> {
    let mut key = document_key_prefix(table);
    push_u64(&mut key, doc_id);
    key
}

fn posting_key_prefix(table: &str) -> Vec<u8> {
    table_prefixed_key(TAG_POSTING, table)
}

fn posting_term_prefix(table: &str, field: &str, term: &str) -> Vec<u8> {
    let mut key = posting_key_prefix(table);
    push_str(&mut key, field);
    push_str(&mut key, term);
    key
}

fn posting_key(table: &str, field: &str, term: &str, doc_id: DocId) -> Vec<u8> {
    let mut key = posting_term_prefix(table, field, term);
    push_u64(&mut key, doc_id);
    key
}

fn doc_length_key_prefix(table: &str) -> Vec<u8> {
    table_prefixed_key(TAG_DOC_LENGTH, table)
}

fn doc_length_doc_prefix(table: &str, doc_id: DocId) -> Vec<u8> {
    let mut key = doc_length_key_prefix(table);
    push_u64(&mut key, doc_id);
    key
}

fn doc_length_key(table: &str, doc_id: DocId, field: &str) -> Vec<u8> {
    let mut key = doc_length_doc_prefix(table, doc_id);
    push_str(&mut key, field);
    key
}

fn field_stats_key_prefix(table: &str) -> Vec<u8> {
    table_prefixed_key(TAG_FIELD_STATS, table)
}

fn field_stats_key(table: &str, field: &str) -> Vec<u8> {
    let mut key = field_stats_key_prefix(table);
    push_str(&mut key, field);
    key
}

fn reverse_posting_key_prefix(table: &str) -> Vec<u8> {
    table_prefixed_key(TAG_REVERSE_POSTING, table)
}

fn reverse_posting_doc_prefix(table: &str, doc_id: DocId) -> Vec<u8> {
    let mut key = reverse_posting_key_prefix(table);
    push_u64(&mut key, doc_id);
    key
}

fn reverse_posting_key(table: &str, doc_id: DocId, field: &str, term: &str) -> Vec<u8> {
    let mut key = reverse_posting_doc_prefix(table, doc_id);
    push_str(&mut key, field);
    push_str(&mut key, term);
    key
}

fn vector_key_prefix(table: &str) -> Vec<u8> {
    table_prefixed_key(TAG_VECTOR, table)
}

fn vector_field_prefix(table: &str, field: &str) -> Vec<u8> {
    let mut key = vector_key_prefix(table);
    push_str(&mut key, field);
    key
}

fn vector_key(table: &str, field: &str, doc_id: DocId) -> Vec<u8> {
    let mut key = vector_field_prefix(table, field);
    push_u64(&mut key, doc_id);
    key
}

/// Document store implemented over [`KeyValueStore`].
#[derive(Clone)]
pub struct KeyValueDocumentStore {
    store: Arc<dyn KeyValueStore>,
    table: String,
}

impl KeyValueDocumentStore {
    pub fn new(store: Arc<dyn KeyValueStore>, table: impl Into<String>) -> Self {
        Self {
            store,
            table: table.into(),
        }
    }
}

impl DocumentStore for KeyValueDocumentStore {
    fn put(&mut self, doc_id: DocId, document: Document) {
        let document: Document = document
            .into_iter()
            .filter(|(_, value)| !matches!(value, Value::Null))
            .collect();
        if let Ok(value) = encode_value(&document) {
            let _ = self.store.put(&document_key(&self.table, doc_id), &value);
        }
    }

    fn get(&self, doc_id: DocId) -> Option<Document> {
        self.store
            .get(&document_key(&self.table, doc_id))
            .ok()
            .flatten()
            .and_then(|bytes| decode_value(&bytes).ok())
    }

    fn delete(&mut self, doc_id: DocId) {
        let _ = self.store.delete(&document_key(&self.table, doc_id));
    }

    fn clear(&mut self) {
        let _ = self.store.delete_prefix(&document_key_prefix(&self.table));
    }

    fn doc_ids(&self) -> Vec<DocId> {
        self.store
            .scan_prefix(&document_key_prefix(&self.table))
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(key, _)| {
                let mut offset = 1;
                let _table = read_str(&key, &mut offset).ok()?;
                read_u64(&key, &mut offset).ok()
            })
            .collect()
    }

    fn len(&self) -> usize {
        self.store
            .scan_prefix(&document_key_prefix(&self.table))
            .map_or(0, |rows| rows.len())
    }

    fn snapshot(&self) -> Arc<dyn DocumentStore> {
        Arc::new(self.clone())
    }
}

/// Inverted index implemented over [`KeyValueStore`].
#[derive(Clone)]
pub struct KeyValueInvertedIndex {
    store: Arc<dyn KeyValueStore>,
    table: String,
    analyzer: Analyzer,
    index_field_analyzers: BTreeMap<FieldName, Analyzer>,
    search_field_analyzers: BTreeMap<FieldName, Analyzer>,
}

impl KeyValueInvertedIndex {
    pub fn new(
        store: Arc<dyn KeyValueStore>,
        table: impl Into<String>,
        analyzer: Analyzer,
    ) -> Self {
        Self {
            store,
            table: table.into(),
            analyzer,
            index_field_analyzers: BTreeMap::new(),
            search_field_analyzers: BTreeMap::new(),
        }
    }

    fn old_doc_lengths(&self, doc_id: DocId) -> BTreeMap<FieldName, u64> {
        self.store
            .scan_prefix(&doc_length_doc_prefix(&self.table, doc_id))
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(key, value)| {
                let mut offset = 1;
                let _table = read_str(&key, &mut offset).ok()?;
                let _doc_id = read_u64(&key, &mut offset).ok()?;
                let field = read_str(&key, &mut offset).ok()?;
                let length = decode_u64_value(&value).ok()?;
                Some((field, length))
            })
            .collect()
    }

    fn old_terms(&self, doc_id: DocId) -> Vec<(FieldName, String)> {
        self.store
            .scan_prefix(&reverse_posting_doc_prefix(&self.table, doc_id))
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(key, _)| {
                let mut offset = 1;
                let _table = read_str(&key, &mut offset).ok()?;
                let _doc_id = read_u64(&key, &mut offset).ok()?;
                let field = read_str(&key, &mut offset).ok()?;
                let term = read_str(&key, &mut offset).ok()?;
                Some((field, term))
            })
            .collect()
    }

    fn set_total_length(
        batch: &mut dyn KeyValueBatch,
        table: &str,
        field: &str,
        value: u64,
    ) -> StorageBackendResult<()> {
        let key = field_stats_key(table, field);
        if value == 0 {
            batch.delete(&key)
        } else {
            batch.put(&key, &u64_value(value))
        }
    }
}

impl InvertedIndex for KeyValueInvertedIndex {
    fn analyzer(&self) -> &Analyzer {
        &self.analyzer
    }

    fn add_document(&mut self, doc_id: DocId, fields: BTreeMap<FieldName, String>) {
        let old_lengths = self.old_doc_lengths(doc_id);
        let old_terms = self.old_terms(doc_id);

        let mut new_lengths = BTreeMap::new();
        let mut new_postings = Vec::new();
        for (field, text) in fields {
            let analyzer = self
                .index_field_analyzers
                .get(&field)
                .unwrap_or(&self.analyzer);
            let tokens = analyzer.analyze(&text);
            new_lengths.insert(field.clone(), tokens.len() as u64);
            let mut term_positions: BTreeMap<String, Vec<u32>> = BTreeMap::new();
            for (pos, token) in tokens.into_iter().enumerate() {
                term_positions.entry(token).or_default().push(pos as u32);
            }
            for (term, mut positions) in term_positions {
                positions.sort_unstable();
                positions.dedup();
                new_postings.push((field.clone(), term, positions));
            }
        }

        let mut fields_to_update = BTreeSet::new();
        fields_to_update.extend(old_lengths.keys().cloned());
        fields_to_update.extend(new_lengths.keys().cloned());

        let mut batch = self.store.batch();
        for (field, term) in old_terms {
            let _ = batch.delete(&posting_key(&self.table, &field, &term, doc_id));
            let _ = batch.delete(&reverse_posting_key(&self.table, doc_id, &field, &term));
        }
        for field in old_lengths.keys() {
            let _ = batch.delete(&doc_length_key(&self.table, doc_id, field));
        }

        for field in fields_to_update {
            let base = self
                .store
                .get(&field_stats_key(&self.table, &field))
                .ok()
                .flatten()
                .and_then(|value| decode_u64_value(&value).ok())
                .unwrap_or(0);
            let old = old_lengths.get(&field).copied().unwrap_or(0);
            let new = new_lengths.get(&field).copied().unwrap_or(0);
            let total = base.saturating_sub(old).saturating_add(new);
            let _ = Self::set_total_length(batch.as_mut(), &self.table, &field, total);
        }

        for (field, length) in &new_lengths {
            let _ = batch.put(
                &doc_length_key(&self.table, doc_id, field),
                &u64_value(*length),
            );
        }
        for (field, term, positions) in new_postings {
            let _ = batch.put(
                &posting_key(&self.table, &field, &term, doc_id),
                &positions_to_blob(&positions),
            );
            let _ = batch.put(
                &reverse_posting_key(&self.table, doc_id, &field, &term),
                &[],
            );
        }
        let _ = batch.commit();
    }

    fn remove_document(&mut self, doc_id: DocId) {
        let old_lengths = self.old_doc_lengths(doc_id);
        let old_terms = self.old_terms(doc_id);
        let mut batch = self.store.batch();
        for (field, term) in old_terms {
            let _ = batch.delete(&posting_key(&self.table, &field, &term, doc_id));
            let _ = batch.delete(&reverse_posting_key(&self.table, doc_id, &field, &term));
        }
        for (field, length) in old_lengths {
            let base = self
                .store
                .get(&field_stats_key(&self.table, &field))
                .ok()
                .flatten()
                .and_then(|value| decode_u64_value(&value).ok())
                .unwrap_or(0);
            let total = base.saturating_sub(length);
            let _ = Self::set_total_length(batch.as_mut(), &self.table, &field, total);
            let _ = batch.delete(&doc_length_key(&self.table, doc_id, &field));
        }
        let _ = batch.commit();
    }

    fn clear(&mut self) {
        let _ = self.store.delete_prefix(&posting_key_prefix(&self.table));
        let _ = self
            .store
            .delete_prefix(&doc_length_key_prefix(&self.table));
        let _ = self
            .store
            .delete_prefix(&field_stats_key_prefix(&self.table));
        let _ = self
            .store
            .delete_prefix(&reverse_posting_key_prefix(&self.table));
    }

    fn get_posting_list(&self, field: &str, term: &str) -> PostingList {
        let entries = self
            .store
            .scan_prefix(&posting_term_prefix(&self.table, field, term))
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(key, value)| {
                let mut offset = 1;
                let _table = read_str(&key, &mut offset).ok()?;
                let _field = read_str(&key, &mut offset).ok()?;
                let _term = read_str(&key, &mut offset).ok()?;
                let doc_id = read_u64(&key, &mut offset).ok()?;
                Some(PostingEntry::new(
                    doc_id,
                    Payload {
                        positions: blob_to_positions(&value),
                        score: 0.0,
                        fields: BTreeMap::new(),
                    },
                ))
            })
            .collect();
        PostingList::from_sorted_unchecked(entries)
    }

    fn doc_freq(&self, field: &str, term: &str) -> u64 {
        self.store
            .scan_prefix(&posting_term_prefix(&self.table, field, term))
            .map_or(0, |rows| rows.len() as u64)
    }

    fn get_doc_length(&self, doc_id: DocId, field: &str) -> u64 {
        self.store
            .get(&doc_length_key(&self.table, doc_id, field))
            .ok()
            .flatten()
            .and_then(|value| decode_u64_value(&value).ok())
            .unwrap_or(0)
    }

    fn get_term_freq(&self, doc_id: DocId, field: &str, term: &str) -> u64 {
        self.store
            .get(&posting_key(&self.table, field, term, doc_id))
            .ok()
            .flatten()
            .map_or(0, |value| (value.len() / 4) as u64)
    }

    fn doc_count(&self) -> u64 {
        self.store
            .scan_prefix(&doc_length_key_prefix(&self.table))
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(key, _)| {
                let mut offset = 1;
                let _table = read_str(&key, &mut offset).ok()?;
                read_u64(&key, &mut offset).ok()
            })
            .collect::<BTreeSet<_>>()
            .len() as u64
    }

    fn total_field_length(&self, field: &str) -> u64 {
        self.store
            .get(&field_stats_key(&self.table, field))
            .ok()
            .flatten()
            .and_then(|value| decode_u64_value(&value).ok())
            .unwrap_or(0)
    }

    fn stats(&self) -> IndexStats {
        let doc_count = self.doc_count();
        let mut stats = IndexStats::default();
        stats.total_docs = doc_count;
        if doc_count > 0 {
            let total = self
                .store
                .scan_prefix(&field_stats_key_prefix(&self.table))
                .unwrap_or_default()
                .into_iter()
                .filter_map(|(_, value)| decode_u64_value(&value).ok())
                .sum::<u64>();
            stats.avg_doc_length = total as f64 / doc_count as f64;
        }
        let mut counts: BTreeMap<(String, String), u64> = BTreeMap::new();
        for (key, _) in self
            .store
            .scan_prefix(&posting_key_prefix(&self.table))
            .unwrap_or_default()
        {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset).ok();
            let Some(field) = read_str(&key, &mut offset).ok() else {
                continue;
            };
            let Some(term) = read_str(&key, &mut offset).ok() else {
                continue;
            };
            *counts.entry((field, term)).or_insert(0) += 1;
        }
        for ((field, term), df) in counts {
            stats.set_doc_freq(field, term, df);
        }
        stats
    }

    fn posting_count(&self, field: Option<&str>) -> u64 {
        self.store
            .scan_prefix(&posting_key_prefix(&self.table))
            .unwrap_or_default()
            .into_iter()
            .filter(|(key, _)| {
                field.is_none_or(|target| {
                    let mut offset = 1;
                    let _table = read_str(key, &mut offset).ok();
                    read_str(key, &mut offset).ok().as_deref() == Some(target)
                })
            })
            .count() as u64
    }

    fn doc_length_count(&self, field: Option<&str>) -> u64 {
        self.store
            .scan_prefix(&doc_length_key_prefix(&self.table))
            .unwrap_or_default()
            .into_iter()
            .filter(|(key, _)| {
                field.is_none_or(|target| {
                    let mut offset = 1;
                    let _table = read_str(key, &mut offset).ok();
                    let _doc_id = read_u64(key, &mut offset).ok();
                    read_str(key, &mut offset).ok().as_deref() == Some(target)
                })
            })
            .count() as u64
    }

    fn term_count(&self, field: Option<&str>) -> u64 {
        self.store
            .scan_prefix(&posting_key_prefix(&self.table))
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(key, _)| {
                let mut offset = 1;
                let _table = read_str(&key, &mut offset).ok()?;
                let current_field = read_str(&key, &mut offset).ok()?;
                if field.is_none_or(|target| target == current_field) {
                    Some(read_str(&key, &mut offset).ok()?)
                } else {
                    None
                }
            })
            .collect::<BTreeSet<_>>()
            .len() as u64
    }

    fn snapshot(&self) -> Arc<dyn InvertedIndex> {
        Arc::new(self.clone())
    }

    fn field_names(&self) -> Vec<FieldName> {
        self.store
            .scan_prefix(&field_stats_key_prefix(&self.table))
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(key, _)| {
                let mut offset = 1;
                let _table = read_str(&key, &mut offset).ok()?;
                read_str(&key, &mut offset).ok()
            })
            .collect()
    }

    fn set_field_analyzer(
        &mut self,
        field: &str,
        analyzer: Analyzer,
        phase: AnalyzerPhase,
    ) -> Result<(), String> {
        match phase {
            AnalyzerPhase::Index => {
                self.index_field_analyzers
                    .insert(field.to_string(), analyzer);
            }
            AnalyzerPhase::Search => {
                self.search_field_analyzers
                    .insert(field.to_string(), analyzer);
            }
            AnalyzerPhase::Both => {
                self.index_field_analyzers
                    .insert(field.to_string(), analyzer.clone());
                self.search_field_analyzers
                    .insert(field.to_string(), analyzer);
            }
        }
        Ok(())
    }

    fn get_field_analyzer(&self, field: &str) -> Analyzer {
        self.index_field_analyzers
            .get(field)
            .cloned()
            .unwrap_or_else(|| self.analyzer.clone())
    }

    fn get_search_analyzer(&self, field: &str) -> Analyzer {
        if let Some(analyzer) = self.search_field_analyzers.get(field) {
            return analyzer.clone();
        }
        if let Some(analyzer) = self.index_field_analyzers.get(field) {
            return analyzer.clone();
        }
        self.analyzer.clone()
    }
}

/// Brute-force vector index implemented over [`KeyValueStore`].
#[derive(Clone)]
pub struct KeyValueVectorIndex {
    store: Arc<dyn KeyValueStore>,
    table: String,
    field: String,
    dimensions: u32,
}

impl KeyValueVectorIndex {
    pub fn new(
        store: Arc<dyn KeyValueStore>,
        table: impl Into<String>,
        field: impl Into<String>,
        dimensions: u32,
    ) -> Self {
        Self {
            store,
            table: table.into(),
            field: field.into(),
            dimensions,
        }
    }

    fn load_all(&self) -> Vec<(DocId, Vec<f32>)> {
        self.store
            .scan_prefix(&vector_field_prefix(&self.table, &self.field))
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(key, value)| {
                let mut offset = 1;
                let _table = read_str(&key, &mut offset).ok()?;
                let _field = read_str(&key, &mut offset).ok()?;
                let doc_id = read_u64(&key, &mut offset).ok()?;
                Some((doc_id, blob_to_vector(&value)))
            })
            .collect()
    }
}

impl VectorIndex for KeyValueVectorIndex {
    fn dimensions(&self) -> u32 {
        self.dimensions
    }

    fn index_kind(&self) -> &'static str {
        "keyvalue-bruteforce"
    }

    fn add(&mut self, doc_id: DocId, vector: Vec<f32>) {
        debug_assert_eq!(
            vector.len() as u32,
            self.dimensions,
            "vector dimension mismatch"
        );
        let _ = self.store.put(
            &vector_key(&self.table, &self.field, doc_id),
            &vector_to_blob(&vector),
        );
    }

    fn delete(&mut self, doc_id: DocId) {
        let _ = self
            .store
            .delete(&vector_key(&self.table, &self.field, doc_id));
    }

    fn clear(&mut self) {
        let _ = self
            .store
            .delete_prefix(&vector_field_prefix(&self.table, &self.field));
    }

    fn search_knn(&self, query: &[f32], k: usize) -> PostingList {
        if k == 0 {
            return PostingList::new();
        }
        let entries = self.load_all();
        let mut scored = entries
            .iter()
            .map(|(doc_id, vector)| (*doc_id, cosine_similarity(query, vector)))
            .collect::<Vec<_>>();
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        scored.truncate(k);
        scored.sort_by_key(|(doc_id, _)| *doc_id);
        PostingList::from_sorted_unchecked(
            scored
                .into_iter()
                .map(|(doc_id, sim)| PostingEntry::new(doc_id, Payload::with_score(f64::from(sim))))
                .collect(),
        )
    }

    fn search_threshold(&self, query: &[f32], threshold: f32) -> PostingList {
        let mut entries = self
            .load_all()
            .into_iter()
            .filter_map(|(doc_id, vector)| {
                let sim = cosine_similarity(query, &vector);
                (sim >= threshold)
                    .then(|| PostingEntry::new(doc_id, Payload::with_score(f64::from(sim))))
            })
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.doc_id);
        PostingList::from_sorted_unchecked(entries)
    }

    fn count(&self) -> usize {
        self.store
            .scan_prefix(&vector_field_prefix(&self.table, &self.field))
            .map_or(0, |rows| rows.len())
    }

    fn snapshot(&self) -> Arc<dyn VectorIndex> {
        Arc::new(self.clone())
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredVertex {
    label: String,
    properties_json: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredEdge {
    source_id: u64,
    target_id: u64,
    label: String,
    properties_json: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredForeignServer {
    fdw_type: String,
    options_json: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredForeignTable {
    server_name: String,
    columns_json: String,
    options_json: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredCatalogIndex {
    index_type: String,
    table_name: String,
    columns_json: String,
    parameters_json: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredColumnStats {
    distinct_count: i64,
    null_count: i64,
    min_value: Option<String>,
    max_value: Option<String>,
    row_count: i64,
    histogram_json: String,
    mcv_values_json: String,
    mcv_frequencies_json: String,
}

fn graph_membership_prefix() -> Vec<u8> {
    key_with_tag(TAG_GRAPH_MEMBERSHIP)
}

fn graph_membership_graph_prefix(graph_name: &str) -> Vec<u8> {
    let mut key = graph_membership_prefix();
    push_str(&mut key, graph_name);
    key
}

fn graph_membership_key(entity_type: &str, entity_id: u64, graph_name: &str) -> Vec<u8> {
    let mut key = graph_membership_graph_prefix(graph_name);
    push_str(&mut key, entity_type);
    push_u64(&mut key, entity_id);
    key
}

fn table_field_analyzer_prefix(table_name: &str) -> Vec<u8> {
    let mut key = key_with_tag(TAG_TABLE_FIELD_ANALYZER);
    push_str(&mut key, table_name);
    key
}

fn table_field_analyzer_key(table_name: &str, field: &str, phase: &str) -> Vec<u8> {
    let mut key = table_field_analyzer_prefix(table_name);
    push_str(&mut key, field);
    push_str(&mut key, phase);
    key
}

fn column_stats_prefix(table_name: &str) -> Vec<u8> {
    let mut key = key_with_tag(TAG_COLUMN_STATS);
    push_str(&mut key, table_name);
    key
}

fn column_stats_key(table_name: &str, column_name: &str) -> Vec<u8> {
    let mut key = column_stats_prefix(table_name);
    push_str(&mut key, column_name);
    key
}

/// Catalog facade implemented over [`KeyValueStore`].
#[derive(Clone)]
pub struct KeyValueCatalog {
    store: Arc<dyn KeyValueStore>,
}

impl KeyValueCatalog {
    pub fn new(store: Arc<dyn KeyValueStore>) -> Self {
        Self { store }
    }

    pub fn store(&self) -> Arc<dyn KeyValueStore> {
        Arc::clone(&self.store)
    }
}

impl CatalogFacade for KeyValueCatalog {
    fn set_metadata(&self, key: &str, value: &str) -> StorageBackendResult<()> {
        self.store
            .put(&single_str_key(TAG_METADATA, key), &string_value(value))
    }

    fn get_metadata(&self, key: &str) -> StorageBackendResult<Option<String>> {
        self.store
            .get(&single_str_key(TAG_METADATA, key))?
            .map(decode_string)
            .transpose()
    }

    fn save_table(&self, schema: &TableSchema) -> StorageBackendResult<()> {
        self.store.put(
            &single_str_key(TAG_TABLE, &schema.name),
            &encode_value(schema)?,
        )
    }

    fn load_tables(&self) -> StorageBackendResult<Vec<TableSchema>> {
        let mut rows = self
            .store
            .scan_prefix(&key_with_tag(TAG_TABLE))?
            .into_iter()
            .map(|(_, value)| decode_value::<TableSchema>(&value))
            .collect::<StorageBackendResult<Vec<_>>>()?;
        rows.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(rows)
    }

    fn drop_table(&self, name: &str) -> StorageBackendResult<()> {
        self.store.delete(&single_str_key(TAG_TABLE, name))
    }

    fn purge_table_data(&self, name: &str) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        batch.delete_prefix(&document_key_prefix(name))?;
        batch.delete_prefix(&posting_key_prefix(name))?;
        batch.delete_prefix(&doc_length_key_prefix(name))?;
        batch.delete_prefix(&field_stats_key_prefix(name))?;
        batch.delete_prefix(&reverse_posting_key_prefix(name))?;
        batch.delete_prefix(&vector_key_prefix(name))?;
        batch.delete_prefix(&column_stats_prefix(name))?;
        batch.commit()
    }

    fn save_model(&self, name: &str, json: &str) -> StorageBackendResult<()> {
        self.store
            .put(&single_str_key(TAG_MODEL, name), &string_value(json))
    }

    fn load_models(&self) -> StorageBackendResult<Vec<(String, String)>> {
        load_single_string_rows(self.store.as_ref(), TAG_MODEL)
    }

    fn load_model(&self, name: &str) -> StorageBackendResult<Option<String>> {
        self.store
            .get(&single_str_key(TAG_MODEL, name))?
            .map(decode_string)
            .transpose()
    }

    fn drop_model(&self, name: &str) -> StorageBackendResult<()> {
        self.store.delete(&single_str_key(TAG_MODEL, name))
    }

    fn save_scoring_params(&self, name: &str, params_json: &str) -> StorageBackendResult<()> {
        self.store.put(
            &single_str_key(TAG_SCORING_PARAMS, name),
            &string_value(params_json),
        )
    }

    fn load_scoring_params(&self, name: &str) -> StorageBackendResult<Option<String>> {
        self.store
            .get(&single_str_key(TAG_SCORING_PARAMS, name))?
            .map(decode_string)
            .transpose()
    }

    fn load_all_scoring_params(&self) -> StorageBackendResult<Vec<(String, String)>> {
        load_single_string_rows(self.store.as_ref(), TAG_SCORING_PARAMS)
    }

    fn drop_scoring_params(&self, name: &str) -> StorageBackendResult<()> {
        self.store.delete(&single_str_key(TAG_SCORING_PARAMS, name))
    }

    fn save_named_graph(&self, name: &str) -> StorageBackendResult<()> {
        self.store.put(&single_str_key(TAG_NAMED_GRAPH, name), &[])
    }

    fn drop_named_graph(&self, name: &str) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        batch.delete(&single_str_key(TAG_NAMED_GRAPH, name))?;
        batch.delete_prefix(&graph_membership_graph_prefix(name))?;
        batch.commit()
    }

    fn load_named_graphs(&self) -> StorageBackendResult<Vec<String>> {
        load_single_keys(self.store.as_ref(), TAG_NAMED_GRAPH)
    }

    fn save_vertex(
        &self,
        vertex_id: u64,
        label: &str,
        properties_json: &str,
    ) -> StorageBackendResult<()> {
        let mut key = key_with_tag(TAG_VERTEX);
        push_u64(&mut key, vertex_id);
        self.store.put(
            &key,
            &encode_value(&StoredVertex {
                label: label.to_string(),
                properties_json: properties_json.to_string(),
            })?,
        )
    }

    fn delete_vertex(&self, vertex_id: u64) -> StorageBackendResult<()> {
        let mut key = key_with_tag(TAG_VERTEX);
        push_u64(&mut key, vertex_id);
        self.store.delete(&key)
    }

    fn load_vertices(&self) -> StorageBackendResult<Vec<(u64, String, String)>> {
        let mut rows = Vec::new();
        for (key, value) in self.store.scan_prefix(&key_with_tag(TAG_VERTEX))? {
            let mut offset = 1;
            let vertex_id = read_u64(&key, &mut offset)?;
            let stored: StoredVertex = decode_value(&value)?;
            rows.push((vertex_id, stored.label, stored.properties_json));
        }
        Ok(rows)
    }

    fn save_edge(
        &self,
        edge_id: u64,
        source_id: u64,
        target_id: u64,
        label: &str,
        properties_json: &str,
    ) -> StorageBackendResult<()> {
        let mut key = key_with_tag(TAG_EDGE);
        push_u64(&mut key, edge_id);
        self.store.put(
            &key,
            &encode_value(&StoredEdge {
                source_id,
                target_id,
                label: label.to_string(),
                properties_json: properties_json.to_string(),
            })?,
        )
    }

    fn delete_edge(&self, edge_id: u64) -> StorageBackendResult<()> {
        let mut key = key_with_tag(TAG_EDGE);
        push_u64(&mut key, edge_id);
        self.store.delete(&key)
    }

    fn load_edges(&self) -> StorageBackendResult<Vec<EdgeRow>> {
        let mut rows = Vec::new();
        for (key, value) in self.store.scan_prefix(&key_with_tag(TAG_EDGE))? {
            let mut offset = 1;
            let edge_id = read_u64(&key, &mut offset)?;
            let stored: StoredEdge = decode_value(&value)?;
            rows.push(EdgeRow {
                edge_id,
                source_id: stored.source_id,
                target_id: stored.target_id,
                label: stored.label,
                properties_json: stored.properties_json,
            });
        }
        Ok(rows)
    }

    fn save_graph_membership(
        &self,
        entity_type: &str,
        entity_id: u64,
        graph_name: &str,
    ) -> StorageBackendResult<()> {
        self.store.put(
            &graph_membership_key(entity_type, entity_id, graph_name),
            &[],
        )
    }

    fn delete_graph_membership(
        &self,
        entity_type: &str,
        entity_id: u64,
        graph_name: &str,
    ) -> StorageBackendResult<()> {
        self.store
            .delete(&graph_membership_key(entity_type, entity_id, graph_name))
    }

    fn delete_graph_membership_for_graph(&self, graph_name: &str) -> StorageBackendResult<()> {
        self.store
            .delete_prefix(&graph_membership_graph_prefix(graph_name))?;
        Ok(())
    }

    fn load_graph_memberships(&self) -> StorageBackendResult<Vec<(String, u64, String)>> {
        let mut rows = Vec::new();
        for (key, _) in self.store.scan_prefix(&graph_membership_prefix())? {
            let mut offset = 1;
            let graph_name = read_str(&key, &mut offset)?;
            let entity_type = read_str(&key, &mut offset)?;
            let entity_id = read_u64(&key, &mut offset)?;
            rows.push((entity_type, entity_id, graph_name));
        }
        Ok(rows)
    }

    fn purge_orphan_graph_entities(&self) -> StorageBackendResult<()> {
        let memberships = self.load_graph_memberships()?;
        let vertex_ids = memberships
            .iter()
            .filter_map(|(ty, id, _)| (ty == "vertex").then_some(*id))
            .collect::<BTreeSet<_>>();
        let edge_ids = memberships
            .iter()
            .filter_map(|(ty, id, _)| (ty == "edge").then_some(*id))
            .collect::<BTreeSet<_>>();
        let mut batch = self.store.batch();
        for (id, _, _) in self.load_vertices()? {
            if !vertex_ids.contains(&id) {
                let mut key = key_with_tag(TAG_VERTEX);
                push_u64(&mut key, id);
                batch.delete(&key)?;
            }
        }
        for edge in self.load_edges()? {
            if !edge_ids.contains(&edge.edge_id) {
                let mut key = key_with_tag(TAG_EDGE);
                push_u64(&mut key, edge.edge_id);
                batch.delete(&key)?;
            }
        }
        batch.commit()
    }

    fn save_analyzer(&self, name: &str, config_json: &str) -> StorageBackendResult<()> {
        self.store.put(
            &single_str_key(TAG_ANALYZER, name),
            &string_value(config_json),
        )
    }

    fn drop_analyzer(&self, name: &str) -> StorageBackendResult<()> {
        self.store.delete(&single_str_key(TAG_ANALYZER, name))
    }

    fn load_analyzers(&self) -> StorageBackendResult<Vec<(String, String)>> {
        load_single_string_rows(self.store.as_ref(), TAG_ANALYZER)
    }

    fn save_table_field_analyzer(
        &self,
        table_name: &str,
        field: &str,
        phase: &str,
        analyzer_name: &str,
    ) -> StorageBackendResult<()> {
        self.store.put(
            &table_field_analyzer_key(table_name, field, phase),
            &string_value(analyzer_name),
        )
    }

    fn drop_table_field_analyzers(&self, table_name: &str) -> StorageBackendResult<()> {
        self.store
            .delete_prefix(&table_field_analyzer_prefix(table_name))?;
        Ok(())
    }

    fn load_table_field_analyzers(
        &self,
    ) -> StorageBackendResult<Vec<(String, String, String, String)>> {
        let mut rows = Vec::new();
        for (key, value) in self
            .store
            .scan_prefix(&key_with_tag(TAG_TABLE_FIELD_ANALYZER))?
        {
            let mut offset = 1;
            let table = read_str(&key, &mut offset)?;
            let field = read_str(&key, &mut offset)?;
            let phase = read_str(&key, &mut offset)?;
            rows.push((table, field, phase, decode_string(value)?));
        }
        rows.sort();
        Ok(rows)
    }

    fn save_foreign_server(
        &self,
        name: &str,
        fdw_type: &str,
        options_json: &str,
    ) -> StorageBackendResult<()> {
        self.store.put(
            &single_str_key(TAG_FOREIGN_SERVER, name),
            &encode_value(&StoredForeignServer {
                fdw_type: fdw_type.to_string(),
                options_json: options_json.to_string(),
            })?,
        )
    }

    fn drop_foreign_server(&self, name: &str) -> StorageBackendResult<()> {
        self.store.delete(&single_str_key(TAG_FOREIGN_SERVER, name))
    }

    fn load_foreign_servers(&self) -> StorageBackendResult<Vec<(String, String, String)>> {
        let mut rows = Vec::new();
        for (key, value) in self.store.scan_prefix(&key_with_tag(TAG_FOREIGN_SERVER))? {
            let mut offset = 1;
            let name = read_str(&key, &mut offset)?;
            let stored: StoredForeignServer = decode_value(&value)?;
            rows.push((name, stored.fdw_type, stored.options_json));
        }
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(rows)
    }

    fn save_foreign_table(
        &self,
        name: &str,
        server_name: &str,
        columns_json: &str,
        options_json: &str,
    ) -> StorageBackendResult<()> {
        self.store.put(
            &single_str_key(TAG_FOREIGN_TABLE, name),
            &encode_value(&StoredForeignTable {
                server_name: server_name.to_string(),
                columns_json: columns_json.to_string(),
                options_json: options_json.to_string(),
            })?,
        )
    }

    fn drop_foreign_table(&self, name: &str) -> StorageBackendResult<()> {
        self.store.delete(&single_str_key(TAG_FOREIGN_TABLE, name))
    }

    fn load_foreign_tables(&self) -> StorageBackendResult<Vec<ForeignTableRow>> {
        let mut rows = Vec::new();
        for (key, value) in self.store.scan_prefix(&key_with_tag(TAG_FOREIGN_TABLE))? {
            let mut offset = 1;
            let name = read_str(&key, &mut offset)?;
            let stored: StoredForeignTable = decode_value(&value)?;
            rows.push(ForeignTableRow {
                name,
                server_name: stored.server_name,
                columns_json: stored.columns_json,
                options_json: stored.options_json,
            });
        }
        rows.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(rows)
    }

    fn save_catalog_index(
        &self,
        name: &str,
        index_type: &str,
        table_name: &str,
        columns_json: &str,
        parameters_json: &str,
    ) -> StorageBackendResult<()> {
        self.store.put(
            &single_str_key(TAG_CATALOG_INDEX, name),
            &encode_value(&StoredCatalogIndex {
                index_type: index_type.to_string(),
                table_name: table_name.to_string(),
                columns_json: columns_json.to_string(),
                parameters_json: parameters_json.to_string(),
            })?,
        )
    }

    fn drop_catalog_index(&self, name: &str) -> StorageBackendResult<()> {
        self.store.delete(&single_str_key(TAG_CATALOG_INDEX, name))
    }

    fn drop_catalog_indexes_for_table(&self, table_name: &str) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        for row in self.load_catalog_indexes()? {
            if row.table_name == table_name {
                batch.delete(&single_str_key(TAG_CATALOG_INDEX, &row.name))?;
            }
        }
        batch.commit()
    }

    fn load_catalog_indexes(&self) -> StorageBackendResult<Vec<CatalogIndexRow>> {
        let mut rows = Vec::new();
        for (key, value) in self.store.scan_prefix(&key_with_tag(TAG_CATALOG_INDEX))? {
            let mut offset = 1;
            let name = read_str(&key, &mut offset)?;
            let stored: StoredCatalogIndex = decode_value(&value)?;
            rows.push(CatalogIndexRow {
                name,
                index_type: stored.index_type,
                table_name: stored.table_name,
                columns_json: stored.columns_json,
                parameters_json: stored.parameters_json,
            });
        }
        rows.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(rows)
    }

    fn save_path_index(
        &self,
        graph_name: &str,
        label_sequences_json: &str,
    ) -> StorageBackendResult<()> {
        self.store.put(
            &single_str_key(TAG_PATH_INDEX, graph_name),
            &string_value(label_sequences_json),
        )
    }

    fn drop_path_index(&self, graph_name: &str) -> StorageBackendResult<()> {
        self.store
            .delete(&single_str_key(TAG_PATH_INDEX, graph_name))
    }

    fn load_path_indexes(&self) -> StorageBackendResult<Vec<(String, String)>> {
        load_single_string_rows(self.store.as_ref(), TAG_PATH_INDEX)
    }

    fn save_column_stats(&self, stats: ColumnStatsInput<'_>) -> StorageBackendResult<()> {
        self.store.put(
            &column_stats_key(stats.table_name, stats.column_name),
            &encode_value(&StoredColumnStats {
                distinct_count: stats.distinct_count,
                null_count: stats.null_count,
                min_value: stats.min_value.map(str::to_string),
                max_value: stats.max_value.map(str::to_string),
                row_count: stats.row_count,
                histogram_json: stats.histogram_json.to_string(),
                mcv_values_json: stats.mcv_values_json.to_string(),
                mcv_frequencies_json: stats.mcv_frequencies_json.to_string(),
            })?,
        )
    }

    fn load_column_stats(&self, table_name: &str) -> StorageBackendResult<Vec<ColumnStatsRow>> {
        let mut rows = Vec::new();
        for (key, value) in self.store.scan_prefix(&column_stats_prefix(table_name))? {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let column_name = read_str(&key, &mut offset)?;
            let stored: StoredColumnStats = decode_value(&value)?;
            rows.push(ColumnStatsRow {
                column_name,
                distinct_count: stored.distinct_count,
                null_count: stored.null_count,
                min_value: stored.min_value,
                max_value: stored.max_value,
                row_count: stored.row_count,
                histogram_json: stored.histogram_json,
                mcv_values_json: stored.mcv_values_json,
                mcv_frequencies_json: stored.mcv_frequencies_json,
            });
        }
        rows.sort_by(|a, b| a.column_name.cmp(&b.column_name));
        Ok(rows)
    }

    fn delete_column_stats(&self, table_name: &str) -> StorageBackendResult<()> {
        self.store.delete_prefix(&column_stats_prefix(table_name))?;
        Ok(())
    }
}

fn load_single_keys(store: &dyn KeyValueStore, tag: u8) -> StorageBackendResult<Vec<String>> {
    let mut rows = store
        .scan_prefix(&key_with_tag(tag))?
        .into_iter()
        .map(|(key, _)| {
            let mut offset = 1;
            read_str(&key, &mut offset)
        })
        .collect::<StorageBackendResult<Vec<_>>>()?;
    rows.sort();
    Ok(rows)
}

fn load_single_string_rows(
    store: &dyn KeyValueStore,
    tag: u8,
) -> StorageBackendResult<Vec<(String, String)>> {
    let mut rows = store
        .scan_prefix(&key_with_tag(tag))?
        .into_iter()
        .map(|(key, value)| {
            let mut offset = 1;
            Ok((read_str(&key, &mut offset)?, decode_string(value)?))
        })
        .collect::<StorageBackendResult<Vec<_>>>()?;
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(rows)
}

/// Persistent storage factory implemented over [`KeyValueStore`].
#[derive(Clone)]
pub struct KeyValueStorageBackend {
    store: Arc<dyn KeyValueStore>,
}

impl KeyValueStorageBackend {
    pub fn new(store: Arc<dyn KeyValueStore>) -> Self {
        Self { store }
    }

    pub fn store(&self) -> Arc<dyn KeyValueStore> {
        Arc::clone(&self.store)
    }
}

impl PersistentStorageBackend for KeyValueStorageBackend {
    fn document_store(&self, table: &str) -> Box<dyn DocumentStore> {
        Box::new(KeyValueDocumentStore::new(Arc::clone(&self.store), table))
    }

    fn inverted_index(&self, table: &str, analyzer: Analyzer) -> Box<dyn InvertedIndex> {
        Box::new(KeyValueInvertedIndex::new(
            Arc::clone(&self.store),
            table,
            analyzer,
        ))
    }

    fn vector_index(
        &self,
        table: &str,
        field: &str,
        dimensions: u32,
        _params: Option<PersistentVectorIndexParams>,
    ) -> Box<dyn VectorIndex> {
        Box::new(KeyValueVectorIndex::new(
            Arc::clone(&self.store),
            table,
            field,
            dimensions,
        ))
    }

    fn begin_transaction(&self) -> StorageBackendResult<()> {
        self.store.begin_transaction()
    }

    fn commit_transaction(&self) -> StorageBackendResult<()> {
        self.store.commit_transaction()
    }

    fn rollback_transaction(&self) -> StorageBackendResult<()> {
        self.store.rollback_transaction()
    }

    fn savepoint(&self, name: &str) -> StorageBackendResult<()> {
        self.store.savepoint(name)
    }

    fn release_savepoint(&self, name: &str) -> StorageBackendResult<()> {
        self.store.release_savepoint(name)
    }

    fn rollback_to_savepoint(&self, name: &str) -> StorageBackendResult<()> {
        self.store.rollback_to_savepoint(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_analysis::standard_analyzer;

    fn store() -> Arc<dyn KeyValueStore> {
        Arc::new(MemoryKeyValueStore::new())
    }

    #[test]
    fn memory_key_value_scan_and_batch_are_ordered_and_atomic() {
        let store = store();
        store.put(b"p/a/2", b"two").unwrap();
        store.put(b"p/a/1", b"one").unwrap();
        store.put(b"p/b/1", b"other").unwrap();
        let rows = store.scan_prefix(b"p/a/").unwrap();
        assert_eq!(
            rows,
            vec![
                (b"p/a/1".to_vec(), b"one".to_vec()),
                (b"p/a/2".to_vec(), b"two".to_vec())
            ]
        );

        let mut batch = store.batch();
        batch.delete(b"p/a/1").unwrap();
        batch.put(b"p/a/3", b"three").unwrap();
        batch.commit().unwrap();
        let rows = store.scan_prefix(b"p/a/").unwrap();
        assert_eq!(
            rows,
            vec![
                (b"p/a/2".to_vec(), b"two".to_vec()),
                (b"p/a/3".to_vec(), b"three".to_vec())
            ]
        );
    }

    #[test]
    fn key_value_document_store_round_trips_documents() {
        let mut docs = KeyValueDocumentStore::new(store(), "articles");
        docs.put(
            7,
            BTreeMap::from([
                ("title".to_string(), Value::Str("Rust".into())),
                ("body".to_string(), Value::Bytes(vec![1, 2, 3])),
            ]),
        );
        assert_eq!(docs.doc_ids(), vec![7]);
        assert_eq!(docs.get_field(7, "title"), Some(Value::Str("Rust".into())));
        assert_eq!(docs.get_field(7, "body"), Some(Value::Bytes(vec![1, 2, 3])));
    }

    #[test]
    fn key_value_inverted_index_replaces_and_removes_documents() {
        let mut index =
            KeyValueInvertedIndex::new(store(), "articles", standard_analyzer("english"));
        index.add_document(1, BTreeMap::from([("title".into(), "rust rust".into())]));
        index.add_document(2, BTreeMap::from([("title".into(), "rust search".into())]));
        assert_eq!(index.doc_freq("title", "rust"), 2);
        assert_eq!(index.get_term_freq(1, "title", "rust"), 2);
        assert_eq!(index.total_field_length("title"), 4);

        index.add_document(1, BTreeMap::from([("title".into(), "sqlite".into())]));
        assert_eq!(index.doc_freq("title", "rust"), 1);
        assert_eq!(index.doc_freq("title", "sqlite"), 1);
        assert_eq!(index.total_field_length("title"), 3);

        index.remove_document(2);
        assert_eq!(index.doc_count(), 1);
        assert_eq!(index.doc_freq("title", "rust"), 0);
        assert_eq!(index.total_field_length("title"), 1);
    }

    #[test]
    fn key_value_catalog_preserves_core_registries() {
        let catalog = KeyValueCatalog::new(store());
        catalog.set_metadata("schema_version", "10").unwrap();
        assert_eq!(
            catalog.get_metadata("schema_version").unwrap().as_deref(),
            Some("10")
        );
        catalog
            .save_table(&TableSchema {
                name: "docs".into(),
                analyzer_json: "{}".into(),
                fts_fields: vec!["title".into()],
                vector_fields: Vec::new(),
                columns_json: "[]".into(),
            })
            .unwrap();
        catalog.save_model("reranker", "{\"model\":1}").unwrap();
        catalog
            .save_scoring_params("bm25", "{\"alpha\":0.5}")
            .unwrap();
        catalog.save_named_graph("g").unwrap();
        catalog.save_vertex(1, "Person", "{}").unwrap();
        catalog.save_graph_membership("vertex", 1, "g").unwrap();

        assert_eq!(catalog.load_tables().unwrap()[0].name, "docs");
        assert_eq!(
            catalog.load_model("reranker").unwrap().as_deref(),
            Some("{\"model\":1}")
        );
        assert_eq!(catalog.load_named_graphs().unwrap(), vec!["g"]);
        assert_eq!(catalog.load_vertices().unwrap()[0].0, 1);
        assert_eq!(
            catalog.load_graph_memberships().unwrap(),
            vec![("vertex".into(), 1, "g".into())]
        );
    }
}
