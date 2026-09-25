//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generation-bound little-endian node slots, logical pages and immutable metadata records.

use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};

mod change;
mod codes;
mod coverage;
mod identity;
mod layout;
mod manifest;
mod node;
mod origin;
mod page;
mod provenance;
mod quantization;
mod record;
mod side;

pub use change::{DiskANNChangeIdentity, CHANGE_IDENTITY_BYTES};
pub use codes::DiskANNCodeBatch;
pub use coverage::{DiskANNBuildCoverage, DiskANNCoverageBuilder};
pub use identity::{DiskANNGeneration, DiskANNVectorVersion};
pub use layout::{DiskANNNodeAddress, DiskANNNodeLayout, DiskANNPageShape};
pub use manifest::{DiskANNArtifactDigests, DiskANNManifest, DiskANNManifestInput};
pub use node::{DiskANNNode, DiskANNNodeInput};
pub use origin::{DiskANNCanonicalOrigin, CANONICAL_ORIGIN_BYTES};
pub use page::{decode_page, encode_page, DiskANNPage};
pub(in crate::diskann_index) use provenance::adjacency_hash;
pub use provenance::DiskANNBuildProvenance;
pub use quantization::{decode_codebook, encode_codebook, DiskANNQuantizationIdentity};
pub use record::artifact_digest;
pub use side::{DiskANNSideBatch, DiskANNSideEntry, DiskANNSideLayout};

pub const NODE_HEADER_BYTES: usize = 64;
pub const PAGE_BYTES: usize = 4096;
pub const PAGE_HEADER_BYTES: usize = 144;
pub const PAGE_PAYLOAD_BYTES: usize = PAGE_BYTES - PAGE_HEADER_BYTES;
pub const PAGE_FORMAT_REVISION: u32 = 1;

fn invalid(message: &str) -> StorageBackendError {
    StorageBackendError::Other(format!("invalid DiskANN encoding: {message}"))
}

fn field<const N: usize>(bytes: &[u8], offset: usize) -> StorageBackendResult<[u8; N]> {
    offset
        .checked_add(N)
        .and_then(|end| bytes.get(offset..end))
        .and_then(|field| field.try_into().ok())
        .ok_or_else(|| invalid("truncated field"))
}

fn zeros(bytes: &[u8], control: &StorageReadControl) -> StorageBackendResult<()> {
    for chunk in bytes.chunks(1024) {
        control.check()?;
        if chunk.iter().any(|&byte| byte != 0) {
            return Err(invalid("nonzero reserved bytes"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
