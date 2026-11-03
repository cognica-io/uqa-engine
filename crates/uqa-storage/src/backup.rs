//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Provider-independent backup image and immutable repository formats.
//!
//! Native local repositories provide immutable packs, verified publication and
//! prepared-image recovery. Source snapshots and restored database activation
//! remain provider-owned operations. Decoding a root alone does not prove that
//! its objects exist, authenticate, or form a complete provider image.

mod error;
mod frame;
mod inventory;
mod records;
#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
mod repository;
mod root;
mod types;

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
pub use repository::{
    BackupLease, BackupOperation, BackupPack, LocalRepository, PackObject, PackRange,
};

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
