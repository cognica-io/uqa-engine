//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared marker identity includes both the committed input and the private overlay.

use super::MergedRecordSnapshot;
use crate::{
    mvcc::{DatabaseId, VersionError},
    read_control::StorageReadControl,
    StorageBackendResult,
};
use sha2::{Digest, Sha256};
use std::sync::OnceLock;

fn process_identity() -> StorageBackendResult<[u8; 16]> {
    static IDENTITY: OnceLock<[u8; 16]> = OnceLock::new();
    if let Some(identity) = IDENTITY.get() {
        return Ok(*identity);
    }
    let identity = crate::catalog::new_nonzero_catalog_identity("record view", "process")?;
    let _ = IDENTITY.set(identity);
    Ok(*IDENTITY.get().expect("initialized process identity"))
}

impl MergedRecordSnapshot {
    /// Fingerprint one shared change marker without loading its value. A private marker must not hide a later committed marker: both inputs contribute, while unrelated commits do not. Private identities include a process incarnation and cannot alias after restart. This is versioned SHA-256 metadata, not a vector-content checksum.
    pub fn marker_fingerprint(
        &self,
        database: DatabaseId,
        key: &[u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<[u8; 32]> {
        control.check()?;
        let mut digest = Sha256::new();
        digest.update(b"uqa-record-marker-v1\0");
        digest.update(database.as_bytes());
        digest.update((key.len() as u64).to_le_bytes());
        digest.update(key);
        match self
            .committed
            .metadata(key, control)
            .map_err(VersionError::into_storage_error)?
        {
            Some(metadata) => {
                digest.update([1, u8::from(metadata.live)]);
                digest.update(
                    metadata
                        .revision
                        .map_or(0, crate::mvcc::CommitSequence::as_u64)
                        .to_le_bytes(),
                );
            }
            None => digest.update([0]),
        }
        let private = self
            .private_keys(key, None, 1, control)
            .map_err(VersionError::into_storage_error)?;
        match private.first().filter(|record| record.key() == key) {
            Some(record) => {
                digest.update([1]);
                digest.update(process_identity()?);
                digest.update(std::process::id().to_le_bytes());
                digest.update(record.revision().as_u64().to_le_bytes());
            }
            None => digest.update([0]),
        }
        control.check()?;
        Ok(digest.finalize().into())
    }
}
