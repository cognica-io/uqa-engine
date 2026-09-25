//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable change identities sort by document; writer allocations never imply visibility order.

use super::{field, invalid, DiskANNVectorVersion};
use crate::mvcc::{DatabaseId, StorageTransactionId, VersionError};
use crate::StorageBackendResult;
use uqa_core::DocId;

pub const CHANGE_IDENTITY_BYTES: usize = 40;

/// One actual canonical mutation. Its fixed-width key is distinct from both generation-local nodes and commit order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskANNChangeIdentity {
    document: DocId,
    version: DiskANNVectorVersion,
}

impl DiskANNChangeIdentity {
    pub fn new(document: DocId, version: DiskANNVectorVersion) -> Self {
        Self { document, version }
    }

    pub fn document(self) -> DocId {
        self.document
    }

    pub fn version(self) -> DiskANNVectorVersion {
        self.version
    }

    /// Big-endian fields group every historical mutation of a document into one seekable range.
    pub fn encode(self) -> [u8; CHANGE_IDENTITY_BYTES] {
        let mut bytes = [0; CHANGE_IDENTITY_BYTES];
        bytes[..8].copy_from_slice(&self.document.to_be_bytes());
        bytes[8..24].copy_from_slice(&self.version.writer().database().as_bytes());
        bytes[24..32].copy_from_slice(&self.version.writer().allocation().to_be_bytes());
        bytes[32..].copy_from_slice(&self.version.revision().to_be_bytes());
        bytes
    }

    pub fn decode(bytes: &[u8]) -> StorageBackendResult<Self> {
        if bytes.len() != CHANGE_IDENTITY_BYTES {
            return Err(invalid("invalid change identity width"));
        }
        let writer = StorageTransactionId::new(
            DatabaseId::from_bytes(field(bytes, 8)?),
            u64::from_be_bytes(field(bytes, 24)?),
        )
        .map_err(VersionError::into_storage_error)?;
        Ok(Self {
            document: u64::from_be_bytes(field(bytes, 0)?),
            version: DiskANNVectorVersion::new(writer, u64::from_be_bytes(field(bytes, 32)?))?,
        })
    }

    /// Exclusive cursor covering all mutation identities for this document, including the terminal document ID.
    pub fn document_end(document: DocId) -> [u8; CHANGE_IDENTITY_BYTES] {
        let mut bytes = [u8::MAX; CHANGE_IDENTITY_BYTES];
        bytes[..8].copy_from_slice(&document.to_be_bytes());
        bytes
    }
}

#[cfg(test)]
mod tests;
