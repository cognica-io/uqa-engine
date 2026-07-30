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
use crate::document_store::{Document, DocumentStore};
use crate::inverted_index::{AnalyzerPhase, InvertedIndex};
use crate::vector_index::{cosine_similarity, validate_vector_values, VectorIndex};
use crate::{StorageBackendError, StorageBackendResult};

#[path = "key_value/catalog.rs"]
mod catalog;
pub use catalog::KeyValueCatalog;

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
const TAG_SCHEMA: u8 = b's';
const TAG_SEQUENCE: u8 = b'q';
const TAG_RELATION: u8 = b'R';
const TAG_VIEW: u8 = b'w';
const TAG_DOCUMENT: u8 = b'd';
const TAG_POSTING: u8 = b'p';
const TAG_DOC_LENGTH: u8 = b'l';
const TAG_FIELD_STATS: u8 = b'f';
const TAG_REVERSE_POSTING: u8 = b'r';
const TAG_VECTOR: u8 = b'v';

/// Prefix for the unambiguous document encoding introduced after JSON arrays
/// became ordinary [`Value::List`] values. A legacy document is plain JSON and
/// therefore cannot start with NUL; the prefix lets reads preserve the old
/// `Bytes`-before-`List` interpretation without misreading newly written lists.
const DOCUMENT_VALUE_V1_PREFIX: &[u8] = b"\0uqa-document-json-v1\0";

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
    fn contains_key(&self, key: &[u8]) -> StorageBackendResult<bool> {
        self.get(key).map(|value| value.is_some())
    }
    fn put(&self, key: &[u8], value: &[u8]) -> StorageBackendResult<()>;
    fn delete(&self, key: &[u8]) -> StorageBackendResult<()>;
    fn scan_prefix(&self, prefix: &[u8]) -> StorageBackendResult<Vec<(Vec<u8>, Vec<u8>)>>;
    fn first_prefix_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
    ) -> StorageBackendResult<Option<(Vec<u8>, Vec<u8>)>> {
        Ok(self
            .scan_prefix(prefix)?
            .into_iter()
            .find(|(key, _)| after.is_none_or(|after| key.as_slice() > after)))
    }
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

    fn contains_key(&self, key: &[u8]) -> StorageBackendResult<bool> {
        Ok(self.inner.lock().map.contains_key(key))
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

    fn first_prefix_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
    ) -> StorageBackendResult<Option<(Vec<u8>, Vec<u8>)>> {
        use std::ops::Bound::{Excluded, Included, Unbounded};

        let inner = self.inner.lock();
        let lower = after.map_or_else(
            || Included(prefix.to_vec()),
            |after| Excluded(after.to_vec()),
        );
        Ok(inner
            .map
            .range((lower, Unbounded))
            .next()
            .filter(|(key, _)| key.starts_with(prefix))
            .map(|(key, value)| (key.clone(), value.clone())))
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

fn encode_document_value(document: &Document) -> StorageBackendResult<Vec<u8>> {
    let body = encode_value(document)?;
    let capacity = DOCUMENT_VALUE_V1_PREFIX
        .len()
        .checked_add(body.len())
        .ok_or_else(|| other_error("KeyValue document encoding size overflow"))?;
    let mut encoded = Vec::new();
    encoded
        .try_reserve_exact(capacity)
        .map_err(|error| other_error(format!("cannot allocate KeyValue document: {error}")))?;
    encoded.extend_from_slice(DOCUMENT_VALUE_V1_PREFIX);
    encoded.extend_from_slice(&body);
    Ok(encoded)
}

fn decode_document_value(bytes: &[u8]) -> StorageBackendResult<Document> {
    if let Some(body) = bytes.strip_prefix(DOCUMENT_VALUE_V1_PREFIX) {
        return decode_value(body);
    }
    decode_legacy_document_value(bytes)
}

/// Decode the unversioned representation written before `Value::Bytes` gained
/// an explicit JSON tag. The historical decoder selected bytes for every
/// empty or byte-range integer array, including nested arrays.
fn decode_legacy_document_value(bytes: &[u8]) -> StorageBackendResult<Document> {
    let serde_json::Value::Object(fields) = serde_json::from_slice(bytes)? else {
        return Err(other_error(
            "persisted KeyValue document is not a JSON object",
        ));
    };
    fields
        .into_iter()
        .map(|(field, value)| Ok((field, decode_legacy_json_value(value)?)))
        .collect()
}

fn decode_legacy_json_value(value: serde_json::Value) -> StorageBackendResult<Value> {
    match value {
        serde_json::Value::Null => Ok(Value::Null),
        serde_json::Value::Bool(value) => Ok(Value::Bool(value)),
        serde_json::Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                Ok(Value::Int(value))
            } else if let Some(value) = value.as_u64() {
                Ok(i64::try_from(value).map_or(Value::Float(value as f64), Value::Int))
            } else if let Some(value) = value.as_f64() {
                Ok(Value::Float(value))
            } else {
                Err(other_error(
                    "persisted KeyValue document number is outside the supported numeric range",
                ))
            }
        }
        serde_json::Value::String(value) => Ok(Value::Str(value)),
        serde_json::Value::Array(values) => {
            let mut decoded = Vec::new();
            decoded.try_reserve_exact(values.len()).map_err(|error| {
                other_error(format!(
                    "cannot allocate legacy KeyValue document array: {error}"
                ))
            })?;
            for value in values {
                decoded.push(decode_legacy_json_value(value)?);
            }

            let mut bytes = Vec::new();
            bytes.try_reserve_exact(decoded.len()).map_err(|error| {
                other_error(format!(
                    "cannot allocate legacy KeyValue document byte array: {error}"
                ))
            })?;
            for value in &decoded {
                let Value::Int(value) = value else {
                    return Ok(Value::List(decoded));
                };
                let Ok(value) = u8::try_from(*value) else {
                    return Ok(Value::List(decoded));
                };
                bytes.push(value);
            }
            Ok(Value::Bytes(bytes))
        }
        serde_json::Value::Object(values) => {
            if values.contains_key("$uqa_type") {
                let tagged = serde_json::Value::Object(values.clone());
                let decoded = serde_json::from_value::<Value>(tagged)?;
                if !matches!(decoded, Value::Map(_)) {
                    return Ok(decoded);
                }
            }
            let mut decoded = BTreeMap::new();
            for (key, value) in values {
                decoded.insert(key, decode_legacy_json_value(value)?);
            }
            Ok(Value::Map(decoded))
        }
    }
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

fn key_segment_length(len: usize) -> StorageBackendResult<u32> {
    len.try_into()
        .map_err(|_| other_error("KeyValue key segment exceeds the u32 on-disk format"))
}

fn push_segment(key: &mut Vec<u8>, segment: &[u8]) -> StorageBackendResult<()> {
    let len = key_segment_length(segment.len())?;
    key.extend_from_slice(&len.to_be_bytes());
    key.extend_from_slice(segment);
    Ok(())
}

fn push_str(key: &mut Vec<u8>, segment: &str) -> StorageBackendResult<()> {
    push_segment(key, segment.as_bytes())
}

fn push_u64(key: &mut Vec<u8>, value: u64) {
    key.extend_from_slice(&value.to_be_bytes());
}

fn read_segment<'a>(key: &'a [u8], offset: &mut usize) -> StorageBackendResult<&'a [u8]> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| other_error("KeyValue key offset overflow"))?;
    let len_bytes: [u8; 4] = key
        .get(*offset..end)
        .ok_or_else(|| other_error("truncated KeyValue key segment length"))?
        .try_into()
        .map_err(|_| other_error("invalid KeyValue key segment length"))?;
    *offset = end;
    let len = usize::try_from(u32::from_be_bytes(len_bytes))
        .map_err(|_| other_error("KeyValue key segment length exceeds usize"))?;
    let end = offset
        .checked_add(len)
        .ok_or_else(|| other_error("KeyValue key segment end overflow"))?;
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
    let end = offset
        .checked_add(8)
        .ok_or_else(|| other_error("KeyValue key offset overflow"))?;
    let bytes: [u8; 8] = key
        .get(*offset..end)
        .ok_or_else(|| other_error("truncated KeyValue u64 key segment"))?
        .try_into()
        .map_err(|_| other_error("invalid KeyValue u64 key segment"))?;
    *offset = end;
    Ok(u64::from_be_bytes(bytes))
}

fn table_prefixed_key(tag: u8, table: &str) -> StorageBackendResult<Vec<u8>> {
    let mut key = key_with_tag(tag);
    push_str(&mut key, table)?;
    Ok(key)
}

fn single_str_key(tag: u8, name: &str) -> StorageBackendResult<Vec<u8>> {
    let mut key = key_with_tag(tag);
    push_str(&mut key, name)?;
    Ok(key)
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

fn positions_to_blob(positions: &[u32]) -> StorageBackendResult<Vec<u8>> {
    let capacity = positions
        .len()
        .checked_mul(std::mem::size_of::<u32>())
        .ok_or_else(|| other_error("posting-position payload size overflow"))?;
    let mut buf = Vec::with_capacity(capacity);
    for p in positions {
        buf.extend_from_slice(&p.to_le_bytes());
    }
    Ok(buf)
}

fn blob_to_positions(blob: &[u8]) -> StorageBackendResult<Vec<u32>> {
    if blob.len() % 4 != 0 {
        return Err(other_error("invalid posting positions KeyValue payload"));
    }
    Ok(blob
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect())
}

fn vector_to_blob(v: &[f32]) -> StorageBackendResult<Vec<u8>> {
    let capacity = v
        .len()
        .checked_mul(std::mem::size_of::<f32>())
        .ok_or_else(|| other_error("vector payload size overflow"))?;
    let mut buf = Vec::with_capacity(capacity);
    for x in v {
        buf.extend_from_slice(&x.to_le_bytes());
    }
    Ok(buf)
}

fn usize_to_u64(value: usize, context: &str) -> StorageBackendResult<u64> {
    u64::try_from(value).map_err(|_| other_error(format!("{context} exceeds u64")))
}

fn validate_vector_ordinal_count(count: u64) -> StorageBackendResult<()> {
    if count > u64::from(u32::MAX) + 1 {
        return Err(other_error("vector ordinal exceeds u32 index format"));
    }
    Ok(())
}

fn blob_to_vector(blob: &[u8]) -> StorageBackendResult<Vec<f32>> {
    if blob.len() % 4 != 0 {
        return Err(other_error("invalid vector KeyValue payload"));
    }
    Ok(blob
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect())
}

fn document_key_prefix(table: &str) -> StorageBackendResult<Vec<u8>> {
    table_prefixed_key(TAG_DOCUMENT, table)
}

fn document_key(table: &str, doc_id: DocId) -> StorageBackendResult<Vec<u8>> {
    let mut key = document_key_prefix(table)?;
    push_u64(&mut key, doc_id);
    Ok(key)
}

fn posting_key_prefix(table: &str) -> StorageBackendResult<Vec<u8>> {
    table_prefixed_key(TAG_POSTING, table)
}

fn posting_field_prefix(table: &str, field: &str) -> StorageBackendResult<Vec<u8>> {
    let mut key = posting_key_prefix(table)?;
    push_str(&mut key, field)?;
    Ok(key)
}

fn posting_term_prefix(table: &str, field: &str, term: &str) -> StorageBackendResult<Vec<u8>> {
    let mut key = posting_field_prefix(table, field)?;
    push_str(&mut key, term)?;
    Ok(key)
}

fn posting_key(
    table: &str,
    field: &str,
    term: &str,
    doc_id: DocId,
) -> StorageBackendResult<Vec<u8>> {
    let mut key = posting_term_prefix(table, field, term)?;
    push_u64(&mut key, doc_id);
    Ok(key)
}

fn doc_length_key_prefix(table: &str) -> StorageBackendResult<Vec<u8>> {
    table_prefixed_key(TAG_DOC_LENGTH, table)
}

fn doc_length_doc_prefix(table: &str, doc_id: DocId) -> StorageBackendResult<Vec<u8>> {
    let mut key = doc_length_key_prefix(table)?;
    push_u64(&mut key, doc_id);
    Ok(key)
}

fn doc_length_key(table: &str, doc_id: DocId, field: &str) -> StorageBackendResult<Vec<u8>> {
    let mut key = doc_length_doc_prefix(table, doc_id)?;
    push_str(&mut key, field)?;
    Ok(key)
}

fn field_stats_key_prefix(table: &str) -> StorageBackendResult<Vec<u8>> {
    table_prefixed_key(TAG_FIELD_STATS, table)
}

fn field_stats_key(table: &str, field: &str) -> StorageBackendResult<Vec<u8>> {
    let mut key = field_stats_key_prefix(table)?;
    push_str(&mut key, field)?;
    Ok(key)
}

fn reverse_posting_key_prefix(table: &str) -> StorageBackendResult<Vec<u8>> {
    table_prefixed_key(TAG_REVERSE_POSTING, table)
}

fn reverse_posting_doc_prefix(table: &str, doc_id: DocId) -> StorageBackendResult<Vec<u8>> {
    let mut key = reverse_posting_key_prefix(table)?;
    push_u64(&mut key, doc_id);
    Ok(key)
}

fn reverse_posting_key(
    table: &str,
    doc_id: DocId,
    field: &str,
    term: &str,
) -> StorageBackendResult<Vec<u8>> {
    let mut key = reverse_posting_doc_prefix(table, doc_id)?;
    push_str(&mut key, field)?;
    push_str(&mut key, term)?;
    Ok(key)
}

fn vector_key_prefix(table: &str) -> StorageBackendResult<Vec<u8>> {
    table_prefixed_key(TAG_VECTOR, table)
}

fn vector_field_prefix(table: &str, field: &str) -> StorageBackendResult<Vec<u8>> {
    let mut key = vector_key_prefix(table)?;
    push_str(&mut key, field)?;
    Ok(key)
}

fn vector_doc_prefix(table: &str, field: &str, doc_id: DocId) -> StorageBackendResult<Vec<u8>> {
    let mut key = vector_field_prefix(table, field)?;
    push_u64(&mut key, doc_id);
    Ok(key)
}

fn vector_key(
    table: &str,
    field: &str,
    doc_id: DocId,
    vector_ordinal: u32,
) -> StorageBackendResult<Vec<u8>> {
    let mut key = vector_doc_prefix(table, field, doc_id)?;
    push_u64(&mut key, u64::from(vector_ordinal));
    Ok(key)
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
    fn put(&mut self, doc_id: DocId, document: Document) -> StorageBackendResult<()> {
        let document: Document = document
            .into_iter()
            .filter(|(_, value)| !matches!(value, Value::Null))
            .collect();
        let value = encode_document_value(&document)?;
        self.store.put(&document_key(&self.table, doc_id)?, &value)
    }

    fn get(&self, doc_id: DocId) -> StorageBackendResult<Option<Document>> {
        self.store
            .get(&document_key(&self.table, doc_id)?)?
            .map(|bytes| decode_document_value(&bytes))
            .transpose()
    }

    fn contains_doc_id(&self, doc_id: DocId) -> StorageBackendResult<bool> {
        self.store.contains_key(&document_key(&self.table, doc_id)?)
    }

    fn delete(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        self.store.delete(&document_key(&self.table, doc_id)?)
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        self.store
            .delete_prefix(&document_key_prefix(&self.table)?)
            .map(|_| ())
    }

    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        let mut out = Vec::new();
        for (key, _) in self.store.scan_prefix(&document_key_prefix(&self.table)?)? {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            out.push(read_u64(&key, &mut offset)?);
        }
        Ok(out)
    }

    fn next_doc_id(&self, after: Option<DocId>) -> StorageBackendResult<Option<DocId>> {
        let prefix = document_key_prefix(&self.table)?;
        let after_key = after
            .map(|doc_id| document_key(&self.table, doc_id))
            .transpose()?;
        let Some((key, _)) = self
            .store
            .first_prefix_after(&prefix, after_key.as_deref())?
        else {
            return Ok(None);
        };
        let mut offset = 1;
        let _table = read_str(&key, &mut offset)?;
        read_u64(&key, &mut offset).map(Some)
    }

    fn len(&self) -> StorageBackendResult<usize> {
        Ok(self
            .store
            .scan_prefix(&document_key_prefix(&self.table)?)?
            .len())
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        Ok(Arc::new(self.clone()))
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

type KeyValueStagedPosting = (FieldName, String, Vec<u32>);
type KeyValueAnalyzedFields = (BTreeMap<FieldName, u64>, Vec<KeyValueStagedPosting>);

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

    fn old_doc_lengths(&self, doc_id: DocId) -> StorageBackendResult<BTreeMap<FieldName, u64>> {
        let mut out = BTreeMap::new();
        for (key, value) in self
            .store
            .scan_prefix(&doc_length_doc_prefix(&self.table, doc_id)?)?
        {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let _doc_id = read_u64(&key, &mut offset)?;
            let field = read_str(&key, &mut offset)?;
            out.insert(field, decode_u64_value(&value)?);
        }
        Ok(out)
    }

    fn old_terms(&self, doc_id: DocId) -> StorageBackendResult<Vec<(FieldName, String)>> {
        let mut out = Vec::new();
        for (key, _) in self
            .store
            .scan_prefix(&reverse_posting_doc_prefix(&self.table, doc_id)?)?
        {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let _doc_id = read_u64(&key, &mut offset)?;
            let field = read_str(&key, &mut offset)?;
            let term = read_str(&key, &mut offset)?;
            out.push((field, term));
        }
        Ok(out)
    }

    fn analyze_fields(
        &self,
        fields: BTreeMap<FieldName, String>,
    ) -> StorageBackendResult<KeyValueAnalyzedFields> {
        let mut lengths = BTreeMap::new();
        let mut postings = Vec::new();
        for (field, text) in fields {
            let analyzer = self
                .index_field_analyzers
                .get(&field)
                .unwrap_or(&self.analyzer);
            let tokens = analyzer.analyze(&text)?;
            let token_count = usize_to_u64(tokens.len(), "document token count")?;
            super::inverted_index::validate_token_position_count(token_count)?;
            lengths.insert(field.clone(), token_count);
            let mut term_positions: BTreeMap<String, Vec<u32>> = BTreeMap::new();
            for (pos, token) in tokens.into_iter().enumerate() {
                term_positions.entry(token).or_default().push(
                    u32::try_from(pos)
                        .map_err(|_| other_error("token position exceeds u32 index format"))?,
                );
            }
            for (term, mut positions) in term_positions {
                positions.sort_unstable();
                positions.dedup();
                postings.push((field.clone(), term, positions));
            }
        }
        Ok((lengths, postings))
    }

    fn set_total_length(
        batch: &mut dyn KeyValueBatch,
        table: &str,
        field: &str,
        value: u64,
    ) -> StorageBackendResult<()> {
        let key = field_stats_key(table, field)?;
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

    fn add_document(
        &mut self,
        doc_id: DocId,
        fields: BTreeMap<FieldName, String>,
    ) -> StorageBackendResult<()> {
        let old_lengths = self.old_doc_lengths(doc_id)?;
        let old_terms = self.old_terms(doc_id)?;

        let (new_lengths, new_postings) = self.analyze_fields(fields)?;

        let mut fields_to_update = BTreeSet::new();
        fields_to_update.extend(old_lengths.keys().cloned());
        fields_to_update.extend(new_lengths.keys().cloned());

        let mut batch = self.store.batch();
        for (field, term) in old_terms {
            batch.delete(&posting_key(&self.table, &field, &term, doc_id)?)?;
            batch.delete(&reverse_posting_key(&self.table, doc_id, &field, &term)?)?;
        }
        for field in old_lengths.keys() {
            batch.delete(&doc_length_key(&self.table, doc_id, field)?)?;
        }

        for field in fields_to_update {
            let base = self
                .store
                .get(&field_stats_key(&self.table, &field)?)?
                .map(|value| decode_u64_value(&value))
                .transpose()?
                .unwrap_or(0);
            let old = old_lengths.get(&field).copied().unwrap_or(0);
            let new = new_lengths.get(&field).copied().unwrap_or(0);
            let total = base
                .checked_sub(old)
                .ok_or_else(|| other_error("stored field length is smaller than document length"))?
                .checked_add(new)
                .ok_or_else(|| other_error("total field length overflow"))?;
            Self::set_total_length(batch.as_mut(), &self.table, &field, total)?;
        }

        for (field, length) in &new_lengths {
            batch.put(
                &doc_length_key(&self.table, doc_id, field)?,
                &u64_value(*length),
            )?;
        }
        for (field, term, positions) in new_postings {
            batch.put(
                &posting_key(&self.table, &field, &term, doc_id)?,
                &positions_to_blob(&positions)?,
            )?;
            batch.put(
                &reverse_posting_key(&self.table, doc_id, &field, &term)?,
                &[],
            )?;
        }
        batch.commit()
    }

    fn remove_document(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        let old_lengths = self.old_doc_lengths(doc_id)?;
        let old_terms = self.old_terms(doc_id)?;
        let mut batch = self.store.batch();
        for (field, term) in old_terms {
            batch.delete(&posting_key(&self.table, &field, &term, doc_id)?)?;
            batch.delete(&reverse_posting_key(&self.table, doc_id, &field, &term)?)?;
        }
        for (field, length) in old_lengths {
            let base = self
                .store
                .get(&field_stats_key(&self.table, &field)?)?
                .map(|value| decode_u64_value(&value))
                .transpose()?
                .unwrap_or(0);
            let total = base.checked_sub(length).ok_or_else(|| {
                other_error("stored field length is smaller than removed document length")
            })?;
            Self::set_total_length(batch.as_mut(), &self.table, &field, total)?;
            batch.delete(&doc_length_key(&self.table, doc_id, &field)?)?;
        }
        batch.commit()
    }

    fn try_rebuild_documents(
        &mut self,
        documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
    ) -> StorageBackendResult<()> {
        let mut staged = BTreeMap::new();
        for (doc_id, fields) in documents {
            if !fields.is_empty() {
                staged.insert(doc_id, self.analyze_fields(fields)?);
            }
        }

        let mut totals: BTreeMap<FieldName, u64> = BTreeMap::new();
        for (lengths, _) in staged.values() {
            for (field, length) in lengths {
                let total = totals.entry(field.clone()).or_insert(0);
                *total = total
                    .checked_add(*length)
                    .ok_or_else(|| other_error("total field length overflow"))?;
            }
        }

        let mut batch = self.store.batch();
        batch.delete_prefix(&posting_key_prefix(&self.table)?)?;
        batch.delete_prefix(&doc_length_key_prefix(&self.table)?)?;
        batch.delete_prefix(&field_stats_key_prefix(&self.table)?)?;
        batch.delete_prefix(&reverse_posting_key_prefix(&self.table)?)?;
        for (field, total) in totals {
            Self::set_total_length(batch.as_mut(), &self.table, &field, total)?;
        }
        for (doc_id, (lengths, postings)) in staged {
            for (field, length) in lengths {
                batch.put(
                    &doc_length_key(&self.table, doc_id, &field)?,
                    &u64_value(length),
                )?;
            }
            for (field, term, positions) in postings {
                batch.put(
                    &posting_key(&self.table, &field, &term, doc_id)?,
                    &positions_to_blob(&positions)?,
                )?;
                batch.put(
                    &reverse_posting_key(&self.table, doc_id, &field, &term)?,
                    &[],
                )?;
            }
        }
        batch.commit()
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        batch.delete_prefix(&posting_key_prefix(&self.table)?)?;
        batch.delete_prefix(&doc_length_key_prefix(&self.table)?)?;
        batch.delete_prefix(&field_stats_key_prefix(&self.table)?)?;
        batch.delete_prefix(&reverse_posting_key_prefix(&self.table)?)?;
        batch.commit()
    }

    fn get_posting_list(&self, field: &str, term: &str) -> StorageBackendResult<PostingList> {
        let mut entries = Vec::new();
        for (key, value) in
            self.store
                .scan_prefix(&posting_term_prefix(&self.table, field, term)?)?
        {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let _field = read_str(&key, &mut offset)?;
            let _term = read_str(&key, &mut offset)?;
            let doc_id = read_u64(&key, &mut offset)?;
            entries.push(PostingEntry::new(
                doc_id,
                Payload {
                    positions: blob_to_positions(&value)?,
                    score: 0.0,
                    fields: BTreeMap::new(),
                },
            ));
        }
        Ok(PostingList::from_sorted_unchecked(entries))
    }

    fn doc_freq(&self, field: &str, term: &str) -> StorageBackendResult<u64> {
        usize_to_u64(
            self.store
                .scan_prefix(&posting_term_prefix(&self.table, field, term)?)?
                .len(),
            "document frequency",
        )
    }

    fn get_doc_length(&self, doc_id: DocId, field: &str) -> StorageBackendResult<u64> {
        Ok(self
            .store
            .get(&doc_length_key(&self.table, doc_id, field)?)?
            .map(|value| decode_u64_value(&value))
            .transpose()?
            .unwrap_or(0))
    }

    fn get_term_freq(&self, doc_id: DocId, field: &str, term: &str) -> StorageBackendResult<u64> {
        self.store
            .get(&posting_key(&self.table, field, term, doc_id)?)?
            .map_or(Ok(0), |value| {
                blob_to_positions(&value)
                    .and_then(|positions| usize_to_u64(positions.len(), "term frequency"))
            })
    }

    fn doc_count(&self) -> StorageBackendResult<u64> {
        let mut doc_ids = BTreeSet::new();
        for (key, _) in self
            .store
            .scan_prefix(&doc_length_key_prefix(&self.table)?)?
        {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            doc_ids.insert(read_u64(&key, &mut offset)?);
        }
        usize_to_u64(doc_ids.len(), "document count")
    }

    fn total_field_length(&self, field: &str) -> StorageBackendResult<u64> {
        Ok(self
            .store
            .get(&field_stats_key(&self.table, field)?)?
            .map(|value| decode_u64_value(&value))
            .transpose()?
            .unwrap_or(0))
    }

    fn vocabulary_terms(&self, field: &str) -> StorageBackendResult<Vec<String>> {
        let mut terms = BTreeSet::new();
        for (key, _) in self.store.scan_prefix(&posting_key_prefix(&self.table)?)? {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let indexed_field = read_str(&key, &mut offset)?;
            let term = read_str(&key, &mut offset)?;
            if indexed_field == field {
                terms.insert(term);
            }
        }
        Ok(terms.into_iter().collect())
    }

    fn stats(&self) -> StorageBackendResult<IndexStats> {
        let doc_count = self.doc_count()?;
        let mut stats = IndexStats::default();
        stats.total_docs = doc_count;
        if doc_count > 0 {
            let mut total = 0_u64;
            for (_, value) in self
                .store
                .scan_prefix(&field_stats_key_prefix(&self.table)?)?
            {
                total = total
                    .checked_add(decode_u64_value(&value)?)
                    .ok_or_else(|| other_error("index total field length overflow"))?;
            }
            stats.avg_doc_length = total as f64 / doc_count as f64;
        }
        let mut counts: BTreeMap<(String, String), u64> = BTreeMap::new();
        for (key, _) in self.store.scan_prefix(&posting_key_prefix(&self.table)?)? {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let field = read_str(&key, &mut offset)?;
            let term = read_str(&key, &mut offset)?;
            let count = counts.entry((field, term)).or_insert(0);
            *count = count
                .checked_add(1)
                .ok_or_else(|| other_error("index document frequency overflow"))?;
        }
        for ((field, term), df) in counts {
            stats.set_doc_freq(field, term, df);
        }
        Ok(stats)
    }

    fn posting_count(&self, field: Option<&str>) -> StorageBackendResult<u64> {
        let mut count = 0_u64;
        for (key, _) in self.store.scan_prefix(&posting_key_prefix(&self.table)?)? {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let indexed_field = read_str(&key, &mut offset)?;
            let _term = read_str(&key, &mut offset)?;
            let _doc_id = read_u64(&key, &mut offset)?;
            if field.is_none_or(|target| target == indexed_field) {
                count = count
                    .checked_add(1)
                    .ok_or_else(|| other_error("posting count overflow"))?;
            }
        }
        Ok(count)
    }

    fn doc_length_count(&self, field: Option<&str>) -> StorageBackendResult<u64> {
        let mut count = 0_u64;
        for (key, _) in self
            .store
            .scan_prefix(&doc_length_key_prefix(&self.table)?)?
        {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let _doc_id = read_u64(&key, &mut offset)?;
            let indexed_field = read_str(&key, &mut offset)?;
            if field.is_none_or(|target| target == indexed_field) {
                count = count
                    .checked_add(1)
                    .ok_or_else(|| other_error("document-length row count overflow"))?;
            }
        }
        Ok(count)
    }

    fn term_count(&self, field: Option<&str>) -> StorageBackendResult<u64> {
        let mut terms = BTreeSet::new();
        for (key, _) in self.store.scan_prefix(&posting_key_prefix(&self.table)?)? {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let current_field = read_str(&key, &mut offset)?;
            let term = read_str(&key, &mut offset)?;
            if field.is_none_or(|target| target == current_field) {
                terms.insert((current_field, term));
            }
        }
        usize_to_u64(terms.len(), "term count")
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn InvertedIndex>> {
        Ok(Arc::new(self.clone()))
    }

    fn field_names(&self) -> StorageBackendResult<Vec<FieldName>> {
        let mut fields = Vec::new();
        for (key, _) in self
            .store
            .scan_prefix(&field_stats_key_prefix(&self.table)?)?
        {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            fields.push(read_str(&key, &mut offset)?);
        }
        Ok(fields)
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

    fn remove_field_analyzers(&mut self, field: &str) -> Result<(), String> {
        self.index_field_analyzers.remove(field);
        self.search_field_analyzers.remove(field);
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

    fn load_all(&self) -> StorageBackendResult<Vec<(DocId, Vec<f32>)>> {
        let mut vectors = Vec::new();
        let mut current_doc = None;
        let mut expected_ordinal = 0_u64;
        for (key, value) in self
            .store
            .scan_prefix(&vector_field_prefix(&self.table, &self.field)?)?
        {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let _field = read_str(&key, &mut offset)?;
            let doc_id = read_u64(&key, &mut offset)?;
            let ordinal = read_u64(&key, &mut offset)?;
            u32::try_from(ordinal)
                .map_err(|_| other_error("persisted vector ordinal exceeds u32 index format"))?;
            if offset != key.len() {
                return Err(other_error("persisted vector key has trailing bytes"));
            }
            if current_doc != Some(doc_id) {
                current_doc = Some(doc_id);
                expected_ordinal = 0;
            }
            if ordinal != expected_ordinal {
                return Err(other_error(format!(
                    "invalid persisted vector ordinal sequence for document {doc_id}: expected {expected_ordinal}, found {ordinal}"
                )));
            }
            expected_ordinal = expected_ordinal
                .checked_add(1)
                .ok_or_else(|| other_error("persisted vector ordinal sequence overflow"))?;
            let vector = blob_to_vector(&value)?;
            self.validate_dimensions(&vector)?;
            vectors.push((doc_id, vector));
        }
        Ok(vectors)
    }

    fn validate_dimensions(&self, vector: &[f32]) -> StorageBackendResult<()> {
        validate_vector_values(self.dimensions, vector).map_err(|error| {
            other_error(format!(
                "invalid vector for {}.{}: {error}",
                self.table, self.field
            ))
        })
    }
}

impl VectorIndex for KeyValueVectorIndex {
    fn dimensions(&self) -> u32 {
        self.dimensions
    }

    fn index_kind(&self) -> &'static str {
        "keyvalue-bruteforce"
    }

    fn add(&mut self, doc_id: DocId, vector: Vec<f32>) -> StorageBackendResult<()> {
        self.add_many(doc_id, vec![vector])
    }

    fn add_many(&mut self, doc_id: DocId, vectors: Vec<Vec<f32>>) -> StorageBackendResult<()> {
        for vector in &vectors {
            self.validate_dimensions(vector)?;
        }
        validate_vector_ordinal_count(usize_to_u64(vectors.len(), "vector count")?)?;
        let mut batch = self.store.batch();
        batch.delete_prefix(&vector_doc_prefix(&self.table, &self.field, doc_id)?)?;
        for (ordinal, vector) in vectors.iter().enumerate() {
            let ordinal = u32::try_from(ordinal)
                .map_err(|_| other_error("vector ordinal exceeds u32 index format"))?;
            batch.put(
                &vector_key(&self.table, &self.field, doc_id, ordinal)?,
                &vector_to_blob(vector)?,
            )?;
        }
        batch.commit()
    }

    fn delete(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        batch.delete_prefix(&vector_doc_prefix(&self.table, &self.field, doc_id)?)?;
        batch.commit()
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        batch.delete_prefix(&vector_field_prefix(&self.table, &self.field)?)?;
        batch.commit()
    }

    fn search_knn(&self, query: &[f32], k: usize) -> StorageBackendResult<PostingList> {
        self.validate_dimensions(query)?;
        if k == 0 {
            return Ok(PostingList::new());
        }
        let entries = self.load_all()?;
        let mut best_by_doc: std::collections::BTreeMap<DocId, f32> =
            std::collections::BTreeMap::new();
        for (doc_id, vector) in &entries {
            let sim = cosine_similarity(query, vector);
            best_by_doc
                .entry(*doc_id)
                .and_modify(|best| {
                    if sim > *best {
                        *best = sim;
                    }
                })
                .or_insert(sim);
        }
        let mut scored = best_by_doc.into_iter().collect::<Vec<_>>();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        scored.truncate(k);
        scored.sort_by_key(|(doc_id, _)| *doc_id);
        Ok(PostingList::from_sorted_unchecked(
            scored
                .into_iter()
                .map(|(doc_id, sim)| PostingEntry::new(doc_id, Payload::with_score(f64::from(sim))))
                .collect(),
        ))
    }

    fn search_threshold(&self, query: &[f32], threshold: f32) -> StorageBackendResult<PostingList> {
        self.validate_dimensions(query)?;
        if !threshold.is_finite() {
            return Err(other_error(format!(
                "vector similarity threshold must be finite, got {threshold}"
            )));
        }
        let mut best_by_doc: std::collections::BTreeMap<DocId, f32> =
            std::collections::BTreeMap::new();
        for (doc_id, vector) in self.load_all()? {
            let sim = cosine_similarity(query, &vector);
            if sim >= threshold {
                best_by_doc
                    .entry(doc_id)
                    .and_modify(|best| {
                        if sim > *best {
                            *best = sim;
                        }
                    })
                    .or_insert(sim);
            }
        }
        let mut entries = best_by_doc
            .into_iter()
            .map(|(doc_id, sim)| PostingEntry::new(doc_id, Payload::with_score(f64::from(sim))))
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.doc_id);
        Ok(PostingList::from_sorted_unchecked(entries))
    }

    fn count(&self) -> StorageBackendResult<usize> {
        Ok(self
            .store
            .scan_prefix(&vector_field_prefix(&self.table, &self.field)?)?
            .len())
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn VectorIndex>> {
        Ok(Arc::new(self.clone()))
    }
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

    #[test]
    fn key_segment_length_rejects_values_outside_the_disk_format() {
        let max_u32 = usize::try_from(u32::MAX).unwrap();
        assert_eq!(key_segment_length(max_u32).unwrap(), u32::MAX);
        if usize::BITS > u32::BITS {
            let error = key_segment_length(max_u32 + 1).unwrap_err();
            assert!(error.to_string().contains("u32 on-disk format"));
        }
    }

    #[test]
    fn key_readers_reject_offset_overflow() {
        let mut offset = usize::MAX;
        let error = read_u64(&[], &mut offset).unwrap_err();
        assert!(error.to_string().contains("offset overflow"));
        assert_eq!(offset, usize::MAX);
    }

    #[test]
    fn vector_ordinal_count_matches_zero_based_u32_format() {
        validate_vector_ordinal_count(u64::from(u32::MAX) + 1).unwrap();
        let error = validate_vector_ordinal_count(u64::from(u32::MAX) + 2).unwrap_err();
        assert!(error.to_string().contains("u32 index format"));
    }
    use crate::catalog::{CatalogFacade, TableSchema};
    use uqa_analysis::{standard_analyzer, Analyzer, Tokenizer};

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
                (
                    "numbers".to_string(),
                    Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)]),
                ),
            ]),
        )
        .unwrap();
        assert_eq!(docs.doc_ids().unwrap(), vec![7]);
        assert_eq!(
            docs.get_field(7, "title").unwrap(),
            Some(Value::Str("Rust".into()))
        );
        assert_eq!(
            docs.get_field(7, "body").unwrap(),
            Some(Value::Bytes(vec![1, 2, 3]))
        );
        assert_eq!(
            docs.get_field(7, "numbers").unwrap(),
            Some(Value::List(vec![
                Value::Int(1),
                Value::Int(2),
                Value::Int(3),
            ]))
        );
    }

    #[test]
    fn key_value_document_codec_preserves_legacy_and_new_array_meanings() {
        let store = store();
        store
            .put(
                &document_key("articles", 7).unwrap(),
                br#"{"legacy_bytes":[1,2],"legacy_empty":[],"legacy_list":[1,300],"nested":[[3,4]]}"#,
            )
            .unwrap();

        let mut docs = KeyValueDocumentStore::new(Arc::clone(&store), "articles");
        docs.put(
            8,
            BTreeMap::from([
                (
                    "new_list".into(),
                    Value::List(vec![Value::Int(1), Value::Int(2)]),
                ),
                ("new_empty".into(), Value::List(Vec::new())),
                ("new_bytes".into(), Value::Bytes(vec![1, 2])),
            ]),
        )
        .unwrap();
        drop(docs);

        let docs = KeyValueDocumentStore::new(Arc::clone(&store), "articles");
        let legacy = docs.get(7).unwrap().unwrap();
        assert_eq!(legacy["legacy_bytes"], Value::Bytes(vec![1, 2]));
        assert_eq!(legacy["legacy_empty"], Value::Bytes(Vec::new()));
        assert_eq!(
            legacy["legacy_list"],
            Value::List(vec![Value::Int(1), Value::Int(300)])
        );
        assert_eq!(
            legacy["nested"],
            Value::List(vec![Value::Bytes(vec![3, 4])])
        );
        let current = docs.get(8).unwrap().unwrap();
        assert_eq!(
            current["new_list"],
            Value::List(vec![Value::Int(1), Value::Int(2)])
        );
        assert_eq!(current["new_empty"], Value::List(Vec::new()));
        assert_eq!(current["new_bytes"], Value::Bytes(vec![1, 2]));
        assert!(store
            .get(&document_key("articles", 8).unwrap())
            .unwrap()
            .unwrap()
            .starts_with(DOCUMENT_VALUE_V1_PREFIX));
    }

    #[test]
    fn key_value_column_rewrites_upgrade_legacy_documents_without_type_loss() {
        let store = store();
        store
            .put(
                &document_key("articles", 7).unwrap(),
                br#"{"legacy_bytes":[1,2],"legacy_empty":[],"legacy_list":[1,300]}"#,
            )
            .unwrap();
        let mut docs = KeyValueDocumentStore::new(Arc::clone(&store), "articles");
        docs.put(
            8,
            BTreeMap::from([
                (
                    "new_list".into(),
                    Value::List(vec![Value::Int(1), Value::Int(2)]),
                ),
                ("new_bytes".into(), Value::Bytes(vec![1, 2])),
                ("drop_me".into(), Value::Str("removed".into())),
            ]),
        )
        .unwrap();

        let catalog = KeyValueCatalog::new(Arc::clone(&store));
        catalog
            .rename_column_data("articles", "legacy_bytes", "renamed_bytes")
            .unwrap();
        catalog.drop_column_data("articles", "drop_me").unwrap();
        drop(docs);

        let docs = KeyValueDocumentStore::new(Arc::clone(&store), "articles");
        let legacy = docs.get(7).unwrap().unwrap();
        assert_eq!(legacy["renamed_bytes"], Value::Bytes(vec![1, 2]));
        assert_eq!(legacy["legacy_empty"], Value::Bytes(Vec::new()));
        assert_eq!(
            legacy["legacy_list"],
            Value::List(vec![Value::Int(1), Value::Int(300)])
        );
        let current = docs.get(8).unwrap().unwrap();
        assert_eq!(
            current["new_list"],
            Value::List(vec![Value::Int(1), Value::Int(2)])
        );
        assert_eq!(current["new_bytes"], Value::Bytes(vec![1, 2]));
        assert!(!current.contains_key("drop_me"));
        for doc_id in [7, 8] {
            assert!(store
                .get(&document_key("articles", doc_id).unwrap())
                .unwrap()
                .unwrap()
                .starts_with(DOCUMENT_VALUE_V1_PREFIX));
        }
    }

    #[test]
    fn key_value_inverted_index_replaces_and_removes_documents() {
        let mut index =
            KeyValueInvertedIndex::new(store(), "articles", standard_analyzer("english"));
        index
            .add_document(1, BTreeMap::from([("title".into(), "rust rust".into())]))
            .unwrap();
        index
            .add_document(2, BTreeMap::from([("title".into(), "rust search".into())]))
            .unwrap();
        assert_eq!(index.doc_freq("title", "rust").unwrap(), 2);
        assert_eq!(index.get_term_freq(1, "title", "rust").unwrap(), 2);
        assert_eq!(index.total_field_length("title").unwrap(), 4);

        index
            .add_document(1, BTreeMap::from([("title".into(), "sqlite".into())]))
            .unwrap();
        assert_eq!(index.doc_freq("title", "rust").unwrap(), 1);
        assert_eq!(index.doc_freq("title", "sqlite").unwrap(), 1);
        assert_eq!(index.total_field_length("title").unwrap(), 3);

        index.remove_document(2).unwrap();
        assert_eq!(index.doc_count().unwrap(), 1);
        assert_eq!(index.doc_freq("title", "rust").unwrap(), 0);
        assert_eq!(index.total_field_length("title").unwrap(), 1);
    }

    #[test]
    fn key_value_add_counter_overflow_is_atomic() {
        let store = store();
        let mut index = KeyValueInvertedIndex::new(
            Arc::clone(&store),
            "articles",
            standard_analyzer("english"),
        );
        index
            .add_document(1, BTreeMap::from([("title".into(), "rust".into())]))
            .unwrap();
        store
            .put(
                &field_stats_key("articles", "title").unwrap(),
                &u64_value(u64::MAX),
            )
            .unwrap();

        let error = index
            .add_document(2, BTreeMap::from([("title".into(), "sqlite".into())]))
            .unwrap_err();
        assert!(error.to_string().contains("total field length overflow"));
        assert_eq!(index.doc_count().unwrap(), 1);
        assert_eq!(index.doc_freq("title", "rust").unwrap(), 1);
        assert_eq!(index.doc_freq("title", "sqlite").unwrap(), 0);
    }

    #[test]
    fn key_value_rebuild_analysis_failure_preserves_old_index() {
        let store = store();
        let mut index = KeyValueInvertedIndex::new(store, "articles", standard_analyzer("english"));
        index
            .add_document(1, BTreeMap::from([("title".into(), "rust".into())]))
            .unwrap();
        let invalid = Analyzer::new(
            Tokenizer::NGram {
                min_gram: 0,
                max_gram: 1,
            },
            Vec::new(),
            Vec::new(),
        );
        index
            .set_field_analyzer("body", invalid, AnalyzerPhase::Index)
            .unwrap();

        let error = index
            .try_rebuild_documents(vec![
                (2, BTreeMap::from([("title".into(), "sqlite".into())])),
                (3, BTreeMap::from([("body".into(), "failure".into())])),
            ])
            .unwrap_err();
        assert!(error.to_string().contains("gram"));
        assert_eq!(index.doc_count().unwrap(), 1);
        assert_eq!(index.doc_freq("title", "rust").unwrap(), 1);
        assert_eq!(index.doc_freq("title", "sqlite").unwrap(), 0);
    }

    #[test]
    fn key_value_field_stats_and_vocabulary_are_field_scoped() {
        let mut index =
            KeyValueInvertedIndex::new(store(), "articles", standard_analyzer("english"));
        index
            .add_document(
                1,
                BTreeMap::from([
                    ("title".into(), "rust search".into()),
                    ("body".into(), "long body text here".into()),
                ]),
            )
            .unwrap();
        index
            .add_document(2, BTreeMap::from([("title".into(), "sqlite".into())]))
            .unwrap();

        let title_stats = index.field_stats("title").unwrap();
        assert_eq!(title_stats.total_docs, 2);
        assert_eq!(title_stats.avg_doc_length, 1.5);
        assert_eq!(
            index.vocabulary_terms("title").unwrap(),
            vec![
                "rust".to_string(),
                "search".to_string(),
                "sqlite".to_string()
            ]
        );
        assert_eq!(index.field_stats("body").unwrap().total_docs, 1);
    }

    #[test]
    fn key_value_vector_reader_rejects_corrupt_ordinal() {
        let store = store();
        let mut key = vector_doc_prefix("articles", "embedding", 1).unwrap();
        push_u64(&mut key, u64::MAX);
        store
            .put(&key, &vector_to_blob(&[1.0, 0.0]).unwrap())
            .unwrap();
        let index = KeyValueVectorIndex::new(store, "articles", "embedding", 2);

        let error = index.search_knn(&[1.0, 0.0], 1).unwrap_err();
        assert!(error.to_string().contains("persisted vector ordinal"));
    }

    #[test]
    fn key_value_catalog_preserves_core_registries() {
        let catalog = KeyValueCatalog::new(store());
        catalog.set_metadata("schema_version", "10").unwrap();
        assert_eq!(
            catalog.get_metadata("schema_version").unwrap().as_deref(),
            Some("10")
        );
        catalog.save_schema("public").unwrap();
        catalog.save_schema("empty_app").unwrap();
        catalog
            .save_table(&TableSchema {
                relation: crate::catalog::RelationIdentity::new("public", "docs"),
                analyzer_json: "{}".into(),
                fts_fields: vec!["title".into()],
                vector_fields: Vec::new(),
                columns_json: "[]".into(),
                constraints_json: String::new(),
            })
            .unwrap();
        catalog.save_model("reranker", "{\"model\":1}").unwrap();
        catalog
            .save_scoring_params("bm25", "{\"alpha\":0.5}")
            .unwrap();
        catalog.save_named_graph("g").unwrap();
        catalog.save_vertex(1, "Person", "{}").unwrap();
        catalog.save_graph_membership("vertex", 1, "g").unwrap();

        assert_eq!(
            catalog.load_tables().unwrap()[0].relation.qualified_name(),
            "public.docs"
        );
        assert_eq!(
            catalog.load_model("reranker").unwrap().as_deref(),
            Some("{\"model\":1}")
        );
        assert_eq!(catalog.load_named_graphs().unwrap(), vec!["g"]);
        assert_eq!(
            catalog.load_schemas().unwrap(),
            vec!["empty_app".to_string(), "public".to_string()]
        );
        assert_eq!(catalog.load_vertices().unwrap()[0].0, 1);
        assert_eq!(
            catalog.load_graph_memberships().unwrap(),
            vec![("vertex".into(), 1, "g".into())]
        );
    }

    #[test]
    fn key_value_column_stats_replace_is_a_complete_batch() {
        let catalog = KeyValueCatalog::new(store());
        catalog
            .save_column_stats(crate::catalog::ColumnStatsInput::basic(
                "docs", "old", 1, 0, None, None, 1,
            ))
            .unwrap();
        let replacement = [
            crate::catalog::ColumnStatsInput::basic("docs", "a", 2, 0, None, None, 3),
            crate::catalog::ColumnStatsInput::basic("docs", "b", 3, 1, None, None, 3),
        ];

        catalog.replace_column_stats("docs", &replacement).unwrap();
        assert_eq!(
            catalog
                .load_column_stats("docs")
                .unwrap()
                .into_iter()
                .map(|row| row.column_name)
                .collect::<Vec<_>>(),
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn key_value_drop_cleans_only_its_legacy_public_alias() {
        let catalog = KeyValueCatalog::new(store());
        catalog.save_schema("public").unwrap();
        catalog.save_schema("app").unwrap();
        for (schema, name) in [("public", "docs"), ("app", "docs")] {
            catalog
                .save_table(&TableSchema {
                    relation: crate::catalog::RelationIdentity::new(schema, name),
                    analyzer_json: "{}".into(),
                    fts_fields: Vec::new(),
                    vector_fields: Vec::new(),
                    columns_json: "[]".into(),
                    constraints_json: String::new(),
                })
                .unwrap();
        }
        for table_name in ["public.docs", "docs", "app.docs"] {
            catalog
                .save_column_stats(crate::catalog::ColumnStatsInput::basic(
                    table_name, "id", 1, 0, None, None, 1,
                ))
                .unwrap();
        }

        catalog.drop_table_and_data("public.docs").unwrap();

        assert!(catalog.load_column_stats("public.docs").unwrap().is_empty());
        assert!(catalog.load_column_stats("docs").unwrap().is_empty());
        assert_eq!(catalog.load_column_stats("app.docs").unwrap().len(), 1);
        assert_eq!(
            catalog.load_tables().unwrap()[0].relation.qualified_name(),
            "app.docs"
        );
    }

    #[test]
    fn key_value_column_lifecycle_rejects_corrupt_catalog_index_columns() {
        let catalog = KeyValueCatalog::new(store());
        catalog
            .save_catalog_index("broken", "btree", "docs", "not-json", "{}")
            .unwrap();

        assert!(matches!(
            catalog.drop_column_data("docs", "title"),
            Err(StorageBackendError::Serde(_))
        ));
        assert_eq!(catalog.load_catalog_indexes().unwrap().len(), 1);
        assert!(matches!(
            catalog.rename_column_data("docs", "title", "headline"),
            Err(StorageBackendError::Serde(_))
        ));
        assert_eq!(
            catalog.load_catalog_indexes().unwrap()[0].columns_json,
            "not-json"
        );
    }
}
