//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Provider-independent canonical origin envelope; raw coordinates remain in their existing canonical layout.

use super::DiskANNVectorVersion;
use crate::mvcc::{DatabaseId, StorageTransactionId, VersionError};
use crate::StorageBackendResult;

const MAGIC: &[u8; 8] = b"UQAVORG1";
pub const CANONICAL_ORIGIN_BYTES: usize = 56;
const BYTES: usize = CANONICAL_ORIGIN_BYTES;

/// Original publishing mutation and complete canonical tensor shape, independent of the provider layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskANNCanonicalOrigin {
    version: DiskANNVectorVersion,
    dimensions: u32,
    count: u64,
}

impl DiskANNCanonicalOrigin {
    pub fn new(
        version: DiskANNVectorVersion,
        dimensions: u32,
        count: u64,
    ) -> StorageBackendResult<Self> {
        validate(dimensions, count)?;
        Ok(Self {
            version,
            dimensions,
            count,
        })
    }

    pub fn version(self) -> DiskANNVectorVersion {
        self.version
    }
    pub fn count(self) -> u64 {
        self.count
    }
    pub fn dimensions(self) -> u32 {
        self.dimensions
    }

    pub fn encode(self) -> [u8; BYTES] {
        let mut bytes = [0; BYTES];
        bytes[..8].copy_from_slice(MAGIC);
        bytes[8..24].copy_from_slice(&self.version.writer().database().as_bytes());
        bytes[24..32].copy_from_slice(&self.version.writer().allocation().to_le_bytes());
        bytes[32..40].copy_from_slice(&self.version.revision().to_le_bytes());
        bytes[40..44].copy_from_slice(&self.dimensions.to_le_bytes());
        bytes[48..56].copy_from_slice(&self.count.to_le_bytes());
        bytes
    }

    pub fn decode(bytes: &[u8], dimensions: u32) -> StorageBackendResult<Self> {
        if bytes.len() != BYTES || &bytes[..8] != MAGIC || bytes[44..48] != [0; 4] {
            return Err(invalid("invalid canonical origin envelope"));
        }
        let read = |offset: usize| -> [u8; 8] {
            bytes[offset..offset + 8]
                .try_into()
                .expect("validated width")
        };
        if bytes[40..44] != dimensions.to_le_bytes() {
            return Err(invalid("canonical origin dimension mismatch"));
        }
        let database = DatabaseId::from_bytes(bytes[8..24].try_into().expect("validated width"));
        let writer = StorageTransactionId::new(database, u64::from_le_bytes(read(24)))
            .map_err(VersionError::into_storage_error)?;
        let version = DiskANNVectorVersion::new(writer, u64::from_le_bytes(read(32)))?;
        let count = u64::from_le_bytes(read(48));
        validate(dimensions, count)?;
        Ok(Self {
            version,
            dimensions,
            count,
        })
    }
}

fn validate(dimensions: u32, count: u64) -> StorageBackendResult<()> {
    if dimensions == 0 || count > (1_u64 << 32) {
        return Err(invalid(
            "invalid canonical tensor dimensions or ordinal count",
        ));
    }
    Ok(())
}

fn invalid(message: &'static str) -> crate::StorageBackendError {
    VersionError::InvalidEncoding(message).into_storage_error()
}

#[cfg(test)]
mod tests;
