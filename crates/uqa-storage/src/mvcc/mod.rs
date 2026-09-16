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
mod memory;
mod types;

pub use commit::{PreparedRecordCommit, PreparedRecordWrite};
pub use history::{RecordHistory, RecordVersion};
pub use memory::{MemoryRecordSnapshot, MemoryVersionStore, ScannedRecord};
pub use types::{CommitSequence, RecordWrite, VersionError, VersionResult};
