//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluated memory vector images retain physical state without executing vector construction.

use super::VectorIndex;
use crate::{
    image_io::ImageOutput, read_control::StorageReadControl, StorageBackendError,
    StorageBackendResult,
};
use uqa_core::memory::{MemoryError, MemoryReservation};

mod exact;
mod input;
pub use input::vector_image_input;
pub(crate) use input::{read_floats, read_from as read_vector_from, Input};
mod snapshot;
pub(super) use exact::{reader as read_exact, write as write_exact};
pub use snapshot::VectorImageSnapshot;
pub(crate) use snapshot::VectorImageSource;

#[cfg(test)]
pub(crate) mod tests;

pub(crate) const MAGIC: &[u8; 8] = b"UQAVEC01";

pub(crate) fn invalid() -> StorageBackendError {
    StorageBackendError::Other("invalid memory vector image".into())
}

/// Decode one complete standalone memory vector image into a private writable owner. Native allocations retain their original allowance through writes, snapshots and source retirement. No training, graph construction, SQL or application callback is invoked.
pub fn read_vector_image(
    reader: &mut dyn std::io::Read,
    control: &StorageReadControl,
) -> StorageBackendResult<Box<dyn VectorIndex>> {
    input::read(reader, control, None)
}

/// Decode an image using the destination's shared temporary-file allowance for future `DiskANN` builds. Import itself does not rebuild an index or consume that allowance.
pub fn read_vector_image_with_temporary(
    reader: &mut dyn std::io::Read,
    control: &StorageReadControl,
    temporary: &crate::diskann_index::build::DiskANNTemporaryBudget,
) -> StorageBackendResult<Box<dyn VectorIndex>> {
    input::read(reader, control, Some(temporary))
}

pub(crate) fn entries<T>(memory: &mut MemoryReservation, count: usize) -> StorageBackendResult<()> {
    memory.grow(
        count
            .checked_mul(size_of::<T>())
            .ok_or(MemoryError::SizeOverflow)?,
    )?;
    Ok(())
}

pub(crate) fn coordinate_bytes(
    values: &mut std::slice::Iter<'_, f32>,
    output: &mut [u8],
) -> StorageBackendResult<usize> {
    let count = values.len().min(output.len() / 4);
    for (bytes, value) in output[..count * 4].chunks_exact_mut(4).zip(values) {
        if !value.is_finite() {
            return Err(invalid());
        }
        bytes.copy_from_slice(&value.to_bits().to_le_bytes());
    }
    Ok(count * 4)
}
