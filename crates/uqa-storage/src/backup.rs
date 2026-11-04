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
#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
mod import;
mod inventory;
mod mapping;
mod records;
#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
mod repository;
mod root;
mod source;
mod types;

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
pub use repository::{
    BackupCaptureReport, BackupCopyReport, BackupImageReader, BackupImageStatistics, BackupLease,
    BackupOperation, BackupPack, BackupReadStep, BackupRestoreImporter, BackupRestoreOperation,
    LocalRepository, PackObject, PackRange, RestoreDirectory, RestoredDatabase,
    VerifiedBackupImage,
};

pub use error::{BackupError, BackupResult};
pub use frame::{read_frame, write_frame, BackupKey, FrameContext, FrameKind, Protection};
#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
pub use import::{BackupRecordImportMapping, MappedRecordImageImportReader, RecordImageImportUnit};
pub use inventory::{InventoryEntry, InventoryReader, InventorySummary, InventoryWriter};
pub use mapping::{BackupRecordMapping, MappedRecordBackupImage};
pub use records::{RecordImageCursor, RecordImageFamily, RecordImageStep, RecordImageUnit};
pub use root::BackupRoot;
pub use source::{BackupImageSource, BackupSourceStatistics, BackupSourceStep, BackupSourceUnit};
pub use types::{
    BackupCapabilities, BackupCut, BackupId, BackupLimits, ImageDigest, ImageFormat, ImageProvider,
    NamespaceId, ObjectId, ObjectSpan, ProtectionId, RepositoryId, RestoreId, SnapshotId,
    UnitAddress, UnitKind,
};

#[cfg(test)]
mod tests;
