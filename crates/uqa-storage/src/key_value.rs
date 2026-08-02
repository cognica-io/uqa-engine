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
    /// Return at most `limit` keys in key order, strictly after `after` when
    /// it is present. Backends should override this method with a key-only,
    /// bounded range scan so cursor consumers neither materialize the entire
    /// prefix nor read values they do not need on every page.
    fn scan_prefix_keys_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
    ) -> StorageBackendResult<Vec<Vec<u8>>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        Ok(self
            .scan_prefix(prefix)?
            .into_iter()
            .filter(|(key, _)| after.is_none_or(|after| key.as_slice() > after))
            .take(limit)
            .map(|(key, _)| key)
            .collect())
    }
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

mod codec;
mod document_store;
mod inverted_index;
mod memory_store;
mod storage_backend;
mod vector_index;

pub use codec::prefix_upper_bound;
pub use document_store::KeyValueDocumentStore;
pub use inverted_index::KeyValueInvertedIndex;
pub use memory_store::MemoryKeyValueStore;
pub use storage_backend::KeyValueStorageBackend;
pub use vector_index::KeyValueVectorIndex;

#[cfg(test)]
mod tests;
