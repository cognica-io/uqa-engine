//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Committed record histories and snapshot retention shared by storage providers.
//!
//! Record validation is independent of SQL tuple locks and command execution. A physical record replacement must carry the revision it replaces, including the revision of a tombstone. Derived indexes additionally need their owning logical mutation rules; an opaque replacement is not an index merge.

mod commit;
mod history;
mod key;
mod memory;
mod overlay;
mod persistence;
mod session;
mod types;
mod view;

pub use commit::{PreparedRecordCommit, PreparedRecordWrite};
pub use history::{RecordHistory, RecordVersion, ScannedRecord, SharedRecordValue};
pub use memory::{MemoryRecordSnapshot, MemoryVersionStore};
pub use overlay::{PrivateRecordChanges, PrivateRecordSnapshot};
pub use persistence::{
    resolve_prepared_receipt, CommitFailure, CommitFingerprint, CommitReceipt, CommitResult,
    CommitStatus, DatabaseId, RecordPage, StorageTransactionId, VersionedPersistence,
};
pub use session::{VersionedKeyValueStore, VersionedSessionOptions};
pub use types::{CommitSequence, RecordWrite, VersionError, VersionResult};
pub use view::{
    retain_record_snapshot, BorrowedRecord, CommittedRecordSnapshot, MergedRecordSnapshot,
    RecordScanVisitor, RecordValueVisitor, ScannedVisibleRecord, VisibleRecord,
};
