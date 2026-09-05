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

use crate::backend::{PersistentStorageBackend, PersistentStorageIdentity};
use crate::document_store::{Document, DocumentMetadata, DocumentStore, StoredDocument};
use crate::inverted_index::{AnalyzerPhase, InvertedIndex};
use crate::vector_index::{
    cosine_similarity, validate_vector_values, VectorIndex, VectorIndexOpenMode, VectorIndexSpec,
};
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
const TAG_POSTING_CLUSTER_SCORE: u8 = b'k';
const TAG_POSTING_CLUSTER_POSITIONS: u8 = b'o';
const TAG_POSTING_DOCUMENT: u8 = b'x';
const TAG_DOC_LENGTH: u8 = b'l';
const TAG_FIELD_STATS: u8 = b'f';
const TAG_REVERSE_POSTING: u8 = b'r';
const TAG_VECTOR: u8 = b'v';
const TAG_BTREE_INDEX: u8 = b'B';
const TAG_BTREE_ENTRY: u8 = b'b';
const TAG_NAMED_BTREE_INDEX: u8 = b'N';
const TAG_NAMED_BTREE_ENTRY: u8 = b'n';
const TAG_IVF_METADATA: u8 = b'I';
const TAG_IVF_CENTROID: u8 = b'i';
const TAG_IVF_ASSIGNMENT: u8 = b'j';
const TAG_HNSW_METADATA: u8 = b'H';
const TAG_HNSW_NODE: u8 = b'h';

/// Prefix for the unambiguous document encoding introduced after JSON arrays
/// became ordinary [`Value::List`] values. A legacy document is plain JSON and
/// therefore cannot start with NUL; the prefix lets reads preserve the old
/// `Bytes`-before-`List` interpretation without misreading newly written lists.
const DOCUMENT_VALUE_V1_PREFIX: &[u8] = b"\0uqa-document-json-v1\0";
const DOCUMENT_VALUE_V2_PREFIX: &[u8] = b"\0uqa-document-record-v2\0";

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
    fn storage_identity(&self) -> StorageBackendResult<Option<PersistentStorageIdentity>> {
        Ok(None)
    }

    /// Open an independent transaction session over the same logical store. The default keeps simple test/custom stores source-compatible while making the missing MVCC capability explicit when a persistent engine needs a committed reader alongside a pinned statement snapshot.
    fn open_session(&self) -> StorageBackendResult<Arc<dyn KeyValueStore>> {
        Err(StorageBackendError::Other(
            "independent sessions are not implemented for this KeyValue store".into(),
        ))
    }

    fn get(&self, key: &[u8]) -> StorageBackendResult<Option<Vec<u8>>>;
    fn contains_key(&self, key: &[u8]) -> StorageBackendResult<bool> {
        self.get(key).map(|value| value.is_some())
    }
    fn put(&self, key: &[u8], value: &[u8]) -> StorageBackendResult<()>;
    fn delete(&self, key: &[u8]) -> StorageBackendResult<()>;
    fn scan_prefix(&self, prefix: &[u8]) -> StorageBackendResult<Vec<(Vec<u8>, Vec<u8>)>>;
    /// Return at most `limit` key/value pairs in key order, strictly after
    /// `after` when supplied. This is the bounded value cursor used by large
    /// format migrations and other paged consumers.
    fn scan_prefix_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
    ) -> StorageBackendResult<Vec<(Vec<u8>, Vec<u8>)>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        Ok(self
            .scan_prefix(prefix)?
            .into_iter()
            .filter(|(key, _)| after.is_none_or(|after| key.as_slice() > after))
            .take(limit)
            .collect())
    }
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

    fn begin_read_transaction(&self) -> StorageBackendResult<()> {
        self.begin_transaction()
    }

    fn begin_upgradeable_transaction(&self) -> StorageBackendResult<()> {
        self.begin_transaction()
    }

    fn in_transaction(&self) -> bool;

    fn transaction_has_written(&self) -> StorageBackendResult<bool>;

    fn change_version(&self) -> StorageBackendResult<Option<u64>> {
        Ok(None)
    }

    fn change_version_monitor_is_nonblocking(&self) -> StorageBackendResult<bool> {
        Ok(true)
    }

    fn pin_transaction_snapshot(&self) -> StorageBackendResult<()> {
        Ok(())
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

mod btree_index;
mod codec;
pub mod conformance;
mod document_store;
mod hnsw_index;
mod hnsw_persistence;
mod index_keys;
mod inverted_index;
mod ivf_index;
mod ivf_persistence;
mod memory_store;
mod storage_backend;
mod vector_index;

pub use codec::prefix_upper_bound;
pub use document_store::KeyValueDocumentStore;
pub use hnsw_index::KeyValueHNSWIndex;
pub use inverted_index::KeyValueInvertedIndex;
pub use ivf_index::KeyValueIVFIndex;
pub use memory_store::MemoryKeyValueStore;
pub use storage_backend::KeyValueStorageBackend;
pub use vector_index::KeyValueVectorIndex;

#[cfg(test)]
mod tests;
