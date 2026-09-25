//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generation-bound little-endian node slots and checksummed logical pages.

use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};

mod identity;
mod layout;
mod node;
mod page;

pub use identity::{DiskANNGeneration, DiskANNVectorVersion};
pub use layout::{DiskANNNodeAddress, DiskANNNodeLayout, DiskANNPageShape};
pub use node::{DiskANNNode, DiskANNNodeInput};
pub use page::{decode_page, encode_page, DiskANNPage};

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
