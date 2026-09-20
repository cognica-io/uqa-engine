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
use crate::document_store::{Document, DocumentStore};
use crate::inverted_index::{AnalyzerPhase, InvertedIndex};
use crate::vector_index::{
    validate_vector_values, VectorIndex, VectorIndexOpenMode, VectorIndexSpec,
};
use crate::{StorageBackendError, StorageBackendResult};

mod catalog;
pub use catalog::KeyValueCatalog;
mod graph_commit;
mod table_owners;
pub use graph_commit::KeyValueGraphRecords;
mod hnsw_records;
mod index_view;
mod ivf_records;
mod maintenance_records;
pub(crate) mod record_json;
pub use maintenance_records::KeyValueMaintenanceRecords;
mod vector_records;
pub use hnsw_records::KeyValueHNSWRecords;
pub use ivf_records::KeyValueIVFRecords;
pub(crate) mod occurrence_records;
pub use occurrence_records::KeyValueOccurrenceRecords;
mod view;
pub use view::{KeyValueMutation, KeyValueRead, KeyValueReadRevision, KeyValueReadScope};

const TAG_METADATA: u8 = b'm';
const TAG_TABLE: u8 = b't';
const TAG_MODEL: u8 = b'M';
const TAG_SCORING_PARAMS: u8 = b'S';
const TAG_NAMED_GRAPH: u8 = b'g';
const TAG_VERTEX: u8 = b'V';
const TAG_EDGE: u8 = b'E';
const TAG_GRAPH_MEMBERSHIP: u8 = b'G';
const TAG_GRAPH_LOOKUP: u8 = b'J';
const TAG_ANALYZER: u8 = b'a';
const TAG_ANALYZER_DESCRIPTOR: u8 = b'D';
const TAG_FIELD_ANALYZER_BINDING: u8 = b'U';
const TAG_TABLE_FIELD_ANALYZER: u8 = b'A';
const TAG_FOREIGN_SERVER: u8 = b'F';
const TAG_FOREIGN_TABLE: u8 = b'T';
const TAG_CATALOG_INDEX: u8 = b'C';
const TAG_PATH_INDEX: u8 = b'P';
const TAG_PATH_INDEX_DATA: u8 = b'Q';
const TAG_COLUMN_STATS: u8 = b'c';
const TAG_SCHEMA: u8 = b's';
const TAG_SEQUENCE: u8 = b'q';
const TAG_RELATION: u8 = b'R';
const TAG_VIEW: u8 = b'w';
const TAG_DOCUMENT: u8 = b'd';
const TAG_POSTING: u8 = b'p';
const TAG_OCCURRENCE_INDEX: u8 = b'e';
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
    /// Require this record's original committed revision at publication without replacing it. This permits independent data writers to share a definition. Stores without commit-time read validation reject this operation; capable wrappers must forward it.
    fn require_unchanged(&mut self, _key: &[u8]) -> StorageBackendResult<()> {
        Err(StorageBackendError::Other(
            "commit-time record requirements are not supported".into(),
        ))
    }
    /// Publish a new revision of an immutable marker, merging concurrent touches of the same value. Structural changes fence the marker and replace their definition; data changes require the definition and touch the marker. The payload must contain only immutable owner/format data. Capable wrappers must forward this operation.
    fn touch_marker(&mut self, _key: &[u8], _value: &[u8]) -> StorageBackendResult<()> {
        Err(StorageBackendError::Other(
            "mergeable revision markers are not supported".into(),
        ))
    }
    /// Stage a durable identifier observation before publishing this batch's records. Successful observations survive later transaction/savepoint rollback. Dropping an unevaluated batch consumes nothing. Stores without autonomous allocation reject this operation; capable wrappers must forward it.
    fn observe_identifier(&mut self, _namespace: &[u8], _value: u64) -> StorageBackendResult<()> {
        Err(StorageBackendError::Other(
            "durable identifier observations are not supported".into(),
        ))
    }
    /// Carry the source's durable identifier watermark into the target before this batch publishes its records. Successful watermark updates survive undo, just like observations. Owning lifecycle code must coordinate the accompanying definition change with data writers. Capable wrappers must forward this namespace operation.
    fn inherit_identifiers(&mut self, _from: &[u8], _to: &[u8]) -> StorageBackendResult<()> {
        Err(StorageBackendError::Other(
            "durable identifier inheritance is not supported".into(),
        ))
    }
    fn put(&mut self, key: &[u8], value: &[u8]) -> StorageBackendResult<()>;
    fn delete(&mut self, key: &[u8]) -> StorageBackendResult<()>;
    fn delete_prefix(&mut self, prefix: &[u8]) -> StorageBackendResult<()>;
    /// Retain already evaluated document input in the same atomic batch as its canonical values and IVF preview. Concurrent wrappers must forward this call.
    fn ivf_mutation(
        &mut self,
        _metadata: &[u8],
        _mutation: crate::ivf_index::IVFMutation<'_>,
    ) -> StorageBackendResult<()> {
        Ok(())
    }
    /// The immutable preview serves private reads; concurrent commit preparation may replace it with an internally recalculated generation.
    fn preview_ivf_record(&mut self, key: &[u8], value: Option<&[u8]>) -> StorageBackendResult<()> {
        match value {
            Some(value) => self.put(key, value),
            None => self.delete(key),
        }
    }
    fn preview_ivf_prefix(&mut self, prefix: &[u8]) -> StorageBackendResult<()> {
        self.delete_prefix(prefix)
    }
    /// Fence each existing IVF definition under this metadata prefix, including definitions with no vectors. Serialized stores already exclude competing writers.
    fn fence_ivf_prefix(&mut self, _prefix: &[u8]) -> StorageBackendResult<()> {
        Ok(())
    }
    /// Retain already evaluated document input in the same atomic batch as its canonical values and HNSW preview. Concurrent wrappers must forward this call.
    fn hnsw_mutation(
        &mut self,
        _metadata: &[u8],
        _mutation: crate::hnsw_index::HNSWMutation<'_>,
    ) -> StorageBackendResult<()> {
        Ok(())
    }
    /// The immutable preview serves private reads; concurrent commit preparation may replace it with an internally recalculated generation.
    fn preview_hnsw_record(
        &mut self,
        key: &[u8],
        value: Option<&[u8]>,
    ) -> StorageBackendResult<()> {
        match value {
            Some(value) => self.put(key, value),
            None => self.delete(key),
        }
    }
    fn preview_hnsw_prefix(&mut self, prefix: &[u8]) -> StorageBackendResult<()> {
        self.delete_prefix(prefix)
    }
    /// Fence each existing HNSW definition under this metadata prefix, including definitions with no vectors. Serialized stores already exclude competing writers.
    fn fence_hnsw_prefix(&mut self, _prefix: &[u8]) -> StorageBackendResult<()> {
        Ok(())
    }
    /// Stage an evaluated occurrence cluster, field total or source marker using the persistence's declared record layout. Source mutations must include their format marker and document guards. Concurrent stores merge only these explicitly typed replacements; ordinary byte writes remain conditional replacements.
    fn replace_occurrence_record(
        &mut self,
        key: &[u8],
        value: Option<&[u8]>,
    ) -> StorageBackendResult<()> {
        match value {
            Some(value) => self.put(key, value),
            None => self.delete(key),
        }
    }
    /// Stage source-owned occurrence-cache invalidation. The source marker in the same batch also drives discovery of caches published after the retained source view.
    fn invalidate_occurrence_prefix(&mut self, prefix: &[u8]) -> StorageBackendResult<()> {
        self.delete_prefix(prefix)
    }
    /// Protect whole-document replacement even when two writers select different fields of an originally absent document.
    fn occurrence_document(&mut self, _table: &str, _document: DocId) -> StorageBackendResult<()> {
        Ok(())
    }
    /// Fence a structural occurrence change, including an empty rebuild, drop, purge or rename. Serialized projections already retain their native mutation boundary.
    fn reset_occurrences(&mut self, _table: &str) -> StorageBackendResult<()> {
        Ok(())
    }
    /// Retain the original precondition and current value of a record, including an absent tombstone, before later replacements. Concurrent wrappers must forward this call; serialized stores already exclude intervening writers.
    fn fence_record(&mut self, _key: &[u8]) -> StorageBackendResult<()> {
        Ok(())
    }
    /// Stage an evaluated statistics-maintenance replacement using the provider's declared layout. Keep ordinary metadata writes canonical.
    fn replace_statistics_maintenance(
        &mut self,
        key: &[u8],
        value: &[u8],
    ) -> StorageBackendResult<()> {
        self.put(key, value)
    }
    /// Record graph-cache dependencies in the same atomic batch. Concurrent MVCC stores must resolve these logical effects before admitting a commit; serialized legacy stores use the ordinary preview writes already included by the catalog.
    fn graph_mutation(
        &mut self,
        _mutation: crate::mvcc::GraphMutation<'_>,
    ) -> StorageBackendResult<()> {
        Ok(())
    }
    /// Maintain read-your-writes for a graph invalidation. MVCC replaces this preview with effects selected from current graph ownership at commit.
    fn preview_graph_invalidation(
        &mut self,
        key: &[u8],
        value: Option<&[u8]>,
    ) -> StorageBackendResult<()> {
        match value {
            Some(value) => self.put(key, value),
            None => self.delete(key),
        }
    }
    /// Replace or clear derived graph cache state. Publishing a completed build must also record `GraphMutation::PublishPath` so its source dependencies are validated.
    fn replace_graph_cache(
        &mut self,
        key: &[u8],
        value: Option<&[u8]>,
    ) -> StorageBackendResult<()> {
        match value {
            Some(value) => self.put(key, value),
            None => self.delete(key),
        }
    }
    fn commit(self: Box<Self>) -> StorageBackendResult<()>;
}

/// Ordered byte-key storage used by Key/Value catalog and index backends.
pub trait KeyValueStore: Send + Sync {
    /// Reclaim committed history outside this session's transaction. Versioned wrappers must forward maintenance; serialized stores that eagerly remove obsolete values may keep the default.
    fn vacuum(&self) -> StorageBackendResult<()> {
        if self.transaction_model().is_versioned() {
            return Err(StorageBackendError::Other(
                "versioned KeyValue maintenance is not implemented by this store".into(),
            ));
        }
        Ok(())
    }

    /// Token shared with the execution that owns this session's writes. Cancellation must not prevent rollback cleanup or diagnostic reads. Versioned wrappers must forward this capability.
    fn write_cancellation(&self) -> Option<uqa_core::CancellationToken> {
        None
    }

    /// Create a transaction-isolated session with the caller's write cancellation and a fresh retention budget.
    fn open_session_with_cancellation(
        &self,
        cancellation: &uqa_core::CancellationToken,
    ) -> StorageBackendResult<Arc<dyn KeyValueStore>> {
        cancellation.check()?;
        if self.transaction_model().is_versioned() {
            return Err(StorageBackendError::Other(
                "cancellable independent KeyValue sessions are not implemented by this store"
                    .into(),
            ));
        }
        self.open_session()
    }

    /// Retain this exact committed/private view in a separate read-only session, including its original logical reader attribution. The returned session cannot publish records or complete the source transaction. Versioned wrappers must forward this capability; opening a newer independent snapshot is not equivalent.
    fn open_retained_read_session(
        &self,
        cancellation: &uqa_core::CancellationToken,
    ) -> StorageBackendResult<Arc<dyn KeyValueStore>> {
        cancellation.check()?;
        Err(StorageBackendError::Other(
            "retained read sessions are not implemented by this KeyValue store".into(),
        ))
    }

    /// Whether writes remain private while independent sessions read and write. Versioned stores must provide command refresh, retained reads, savepoint undo and conditional publication under one reported affinity.
    fn transaction_model(&self) -> crate::StorageTransactionModel {
        crate::StorageTransactionModel::ProviderSerialized
    }

    /// Durable reservations outside private record undo. Serialized custom stores may omit this capability; wrappers over a capable store must forward it.
    fn identifier_allocator(&self) -> Option<&dyn crate::mvcc::IdentifierAllocator> {
        None
    }

    /// Evaluate a compound read once against one fixed committed/private view. The callback must use the supplied reader and must not reenter this session. Stores without this capability reject it explicitly.
    fn with_read_view(&self, _read: &mut KeyValueReadScope<'_>) -> StorageBackendResult<()> {
        Err(StorageBackendError::Other(
            "compound KeyValue reads are not supported by this store".into(),
        ))
    }

    /// Evaluate once against the same view that supplies write preconditions, then atomically stage the batch. Errors and unwinds discard that batch; failed publication retains the evaluated attempt. The callback must not reenter this session or perform transaction control.
    fn with_mutation(&self, _mutate: &mut KeyValueMutation<'_>) -> StorageBackendResult<()> {
        Err(StorageBackendError::Other(
            "atomic KeyValue evaluation is not supported by this store".into(),
        ))
    }

    /// Identity of the transaction context shared by this store's catalog and data handles. Independent sessions must report different identities, even over the same file.
    fn transaction_affinity(&self) -> Option<crate::StorageSessionAffinity> {
        None
    }

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
    /// Visit one borrowed value under the retained read. Providers reserve any temporary encoded payload before fetching it; callbacks must not reenter the store.
    fn visit_value(
        &self,
        _key: &[u8],
        control: &crate::read_control::StorageReadControl,
        _visit: &mut crate::read_control::ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        control.check()?;
        Err(StorageBackendError::Other(
            "controlled value reads are not supported by this KeyValue store".into(),
        ))
    }
    /// Visit at most `limit` entries in key order, strictly after `after` when supplied. Encoded values remain borrowed from their provider owner and callbacks must not reenter the store.
    fn visit_prefix_after(
        &self,
        _prefix: &[u8],
        _after: Option<&[u8]>,
        _limit: usize,
        control: &crate::read_control::StorageReadControl,
        _visit: &mut crate::read_control::KeyValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        control.check()?;
        Err(StorageBackendError::Other(
            "controlled prefix reads are not supported by this KeyValue store".into(),
        ))
    }
    /// Test prefix existence under the read allowance. Providers that materialize values should implement a key-only probe.
    fn contains_prefix_budgeted(
        &self,
        prefix: &[u8],
        control: &crate::read_control::StorageReadControl,
    ) -> StorageBackendResult<bool> {
        let mut found = false;
        self.visit_prefix_after(prefix, None, 1, control, &mut |_, _| {
            found = true;
            Ok(())
        })?;
        control.check()?;
        Ok(found)
    }

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

    /// Advance an active logical transaction's committed view while preserving its private writes and retained readers. The caller chooses command boundaries allowed by SQL isolation. Stores without this capability must reject the request; ending or replaying the caller's transaction is not a valid fallback.
    fn refresh_transaction_snapshot(
        &self,
        _cancellation: &uqa_core::CancellationToken,
    ) -> StorageBackendResult<()> {
        Err(StorageBackendError::Other(
            "transaction snapshot refresh is not supported by this KeyValue store".into(),
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
pub mod occurrence_format;
mod occurrence_keys;
mod storage_backend;
mod vector_index;

pub use codec::prefix_upper_bound;
pub use document_store::KeyValueDocumentStore;
pub use hnsw_index::KeyValueHNSWIndex;
pub use inverted_index::{KeyValueInvertedIndex, OccurrenceStorage};
pub use ivf_index::KeyValueIVFIndex;
pub use memory_store::MemoryKeyValueStore;
pub use storage_backend::KeyValueStorageBackend;
pub use vector_index::KeyValueVectorIndex;

#[cfg(test)]
mod tests;
