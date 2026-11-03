//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Provider-independent backup image and immutable repository formats.
//!
//! These codecs do not establish a consistent source snapshot, publish a durable
//! backup, or activate a restored database. Those operations require their owning
//! provider and repository state machines. Decoding a root is not proof that its
//! referenced objects exist, authenticate, or form a complete provider image.

mod error;
mod frame;
mod inventory;
mod records;
mod root;
mod types;

pub use error::{BackupError, BackupResult};
pub use frame::{read_frame, write_frame, BackupKey, FrameContext, FrameKind, Protection};
pub use inventory::{InventoryEntry, InventoryReader, InventorySummary, InventoryWriter};
pub use records::{RecordImageCursor, RecordImageFamily, RecordImageStep, RecordImageUnit};
pub use root::BackupRoot;
pub use types::{
    BackupCapabilities, BackupCut, BackupId, BackupLimits, ImageDigest, ImageFormat, ImageProvider,
    NamespaceId, ObjectId, ObjectSpan, ProtectionId, RepositoryId, SnapshotId, UnitAddress,
    UnitKind,
};

#[cfg(test)]
mod tests;
