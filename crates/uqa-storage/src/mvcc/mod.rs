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
mod guards;
mod history;
mod hnsw;
mod identifiers;
mod ivf;
mod key;
mod markers;
mod memory;
mod notifications;
mod occurrence;
pub use notifications::{NotificationRecordLayout, NOTIFICATION_PUBLICATION_KEY};
mod outcome;
mod overlay;
mod persistence;
mod projection;
mod receipts;
mod resolution;
mod restore;
mod retention;
mod serializable;
mod session;
mod types;
mod vector;
mod view;

pub use commit::{PreparedRecordCommit, PreparedRecordWrite};
pub use graph::{GraphMutation, GraphRecordKey, GraphRecordLayout};
pub use guards::verify_revision_guards;
pub use history::{RecordHistory, RecordVersion, ScannedRecord, SharedRecordValue};
pub use hnsw::{HNSWRecordHeader, HNSWRecordKey, HNSWRecordLayout, HNSWRecordValue};
pub use identifiers::{
    reserve_identifier_workspace, verify_identifier_allocations, verify_identifier_batches,
    IdentifierAllocation, IdentifierAllocator, IdentifierRequest,
};
pub use ivf::{IVFRecordHeader, IVFRecordKey, IVFRecordLayout, IVFRecordValue};
pub use memory::{MemoryRecordSnapshot, MemoryVersionStore};
pub use occurrence::{
    OccurrenceRecordKind, OccurrenceRecordLayout, OccurrenceRecordValue, OccurrenceRelatedKey,
};
pub use outcome::{TransactionCompletionError, TransactionOutcome, TransactionOutcomeId};
pub use overlay::{
    PrivateRecordChanges, PrivateRecordKey, PrivateRecordRevision, PrivateRecordSnapshot,
};
pub use persistence::{
    resolve_prepared_receipt, CommitErrorOutcome, CommitFailure, CommitFingerprint, CommitReceipt,
    CommitResult, CommitStatus, DatabaseId, RecordPage, StorageMutationOrigin,
    StorageTransactionId, VersionedPersistence,
};
pub use receipts::{
    receipt_lease_id, ReceiptAcknowledgement, RetainedTransactionAllocation,
    DEFAULT_RECEIPT_RETENTION_LIMIT,
};
pub use restore::DatabaseRestore;
pub use retention::{
    verify_version_reclamation, ReclamationHorizon, SnapshotLease, SnapshotLeaseTransport,
    SnapshotRegistry,
};
pub use serializable::{
    admit_serializable, LocalSerializableLeases, LocalSerializableState, SafeSnapshot,
    SerializableCheckpointKey, SerializableCheckpointRecord, SerializableCoordinator,
    SerializableGraph, SerializableKeySpace, SerializableLeases, SerializableOperation,
    SerializableParticipant, SerializablePredicate, SerializablePublication, SerializableStatus,
    SerializableTransactionId, SerializableWriteMark,
};
pub use session::{
    SerializableReadContext, SerializableSession, SerializableSnapshotCapture,
    SerializableSnapshotOptions, VersionedKeyValueStore, VersionedSessionOptions,
};
pub use types::{CommitSequence, RecordWrite, VersionError, VersionResult};
pub use view::{
    retain_record_snapshot, BorrowedRecord, CommittedRecordSnapshot, MergedRecordSnapshot,
    RecordKeyVisitor, RecordMetadata, RecordScanVisitor, RecordValueVisitor, ScannedVisibleRecord,
    VisibleRecord,
};

mod maintenance;
pub use maintenance::MaintenanceRecordLayout;
