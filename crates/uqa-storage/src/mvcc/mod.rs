//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Committed record histories and snapshot retention shared by storage providers.
//!
//! Record validation is independent of SQL tuple locks and command execution. A physical record replacement must carry the revision it replaces, including the revision of a tombstone. Derived indexes additionally need their owning logical mutation rules; an opaque replacement is not an index merge.

pub(crate) mod commit;
mod graph;
mod history;
mod hnsw;
mod ivf;
mod key;
mod memory;
mod occurrence;
mod overlay;
mod persistence;
mod projection;
mod session;
mod types;
mod vector;
mod view;

pub use commit::{PreparedRecordCommit, PreparedRecordWrite};
pub use graph::{GraphMutation, GraphRecordKey, GraphRecordLayout};
pub use history::{RecordHistory, RecordVersion, ScannedRecord, SharedRecordValue};
pub use hnsw::{HNSWRecordHeader, HNSWRecordKey, HNSWRecordLayout, HNSWRecordValue};
pub use ivf::{IVFRecordHeader, IVFRecordKey, IVFRecordLayout, IVFRecordValue};
pub use memory::{MemoryRecordSnapshot, MemoryVersionStore};
pub use occurrence::{
    OccurrenceRecordKind, OccurrenceRecordLayout, OccurrenceRecordValue, OccurrenceRelatedKey,
};
pub use overlay::{
    PrivateRecordChanges, PrivateRecordKey, PrivateRecordRevision, PrivateRecordSnapshot,
};
pub use persistence::{
    resolve_prepared_receipt, CommitErrorOutcome, CommitFailure, CommitFingerprint, CommitReceipt,
    CommitResult, CommitStatus, DatabaseId, RecordPage, StorageTransactionId, VersionedPersistence,
};
pub use session::{VersionedKeyValueStore, VersionedSessionOptions};
pub use types::{CommitSequence, RecordWrite, VersionError, VersionResult};
pub use view::{
    retain_record_snapshot, BorrowedRecord, CommittedRecordSnapshot, MergedRecordSnapshot,
    RecordKeyVisitor, RecordMetadata, RecordScanVisitor, RecordValueVisitor, ScannedVisibleRecord,
    VisibleRecord,
};
