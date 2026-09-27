//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Calibration metadata describes the actual retained physical and canonical selection.

use super::{
    format::DiskANNManifest,
    pages::{read_manifest, DiskANNPageSource, DiskANNReadCapabilities, DiskANNReadLimits},
    DiskANNCanonicalRead,
};
use crate::{
    read_control::StorageReadControl, vector_index::DiskANNIndexParams, StorageBackendResult,
};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskANNQueryMetadata {
    pub manifest: DiskANNManifest,
    /// None is an unverifiable canonical selection, never permission to trust a caller's version label.
    pub corpus_fingerprint: Option<[u8; 32]>,
    pub read_limits: DiskANNReadLimits,
    pub read_capabilities: DiskANNReadCapabilities,
}

impl DiskANNQueryMetadata {
    /// Read only the bounded manifest and canonical identity from an already associated provider view. This validates the selected definition, not graph/PQ/origin bodies; opening an executable reader retains its complete validation. No vector query or logical observation is performed.
    pub fn capture(
        canonical: &dyn DiskANNCanonicalRead,
        source: &dyn DiskANNPageSource,
        parameters: DiskANNIndexParams,
        limits: DiskANNReadLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        canonical.check_control(control)?;
        let manifest = read_manifest(
            source,
            canonical.dimensions(),
            parameters,
            limits.max_record_bytes,
            control,
        )?;
        let corpus_fingerprint = canonical.corpus_fingerprint(control)?;
        canonical.check_control(control)?;
        Ok(Self {
            manifest,
            corpus_fingerprint,
            read_limits: limits,
            read_capabilities: source.capabilities(),
        })
    }

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
