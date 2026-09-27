//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Calibration metadata describes the actual retained physical and canonical selection.

use super::format::DiskANNManifest;
use crate::{read_control::StorageReadControl, StorageBackendResult};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskANNQueryMetadata {
    pub manifest: DiskANNManifest,
    /// None is an unverifiable canonical selection, never permission to trust a caller's version label.
    pub corpus_fingerprint: Option<[u8; 32]>,
}

impl DiskANNQueryMetadata {
    /// Versioned SHA-256 of the complete validated manifest, including physical incarnations, graph/PQ generation, build revisions and effective candidate-selection settings.
    pub fn index_fingerprint(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<[u8; 32]> {
        control.check()?;
        let mut digest = Sha256::new();
        digest.update(b"uqa-diskann-index-v1\0");
        digest.update(&*self.manifest.encode(control)?);
        control.check()?;
        Ok(digest.finalize().into())
    }
}
