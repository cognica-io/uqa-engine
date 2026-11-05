//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluated memory vector images retain physical state without executing vector construction.

use super::VectorIndex;
use crate::{
    image_io::{ImageInput, ImageOutput},
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};
use uqa_core::memory::{Budgeted, BudgetedVec, MemoryError, MemoryReservation};

mod exact;
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

/// Decode one complete standalone memory vector image into a private candidate. The original allowance owns its decoded state until the value and lease transfer together to an activation owner. No training, graph construction, SQL or application callback is invoked.
pub fn read_vector_image(
    reader: &mut dyn std::io::Read,
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<Box<dyn VectorIndex>>> {
    read_with_temporary(reader, control, None)
}

/// Decode an image using the destination's shared temporary-file allowance for future `DiskANN` builds. Import itself does not rebuild an index or consume that allowance.
pub fn read_vector_image_with_temporary(
    reader: &mut dyn std::io::Read,
    control: &StorageReadControl,
    temporary: &crate::diskann_index::build::DiskANNTemporaryBudget,
) -> StorageBackendResult<Budgeted<Box<dyn VectorIndex>>> {
    read_with_temporary(reader, control, Some(temporary))
}

fn read_with_temporary(
    reader: &mut dyn std::io::Read,
    control: &StorageReadControl,
    temporary: Option<&crate::diskann_index::build::DiskANNTemporaryBudget>,
) -> StorageBackendResult<Budgeted<Box<dyn VectorIndex>>> {
    control.check()?;
    let mut input = ImageInput { reader, control };
    if &input.array::<8>()? != MAGIC {
        return Err(invalid());
    }
    let kind = input.array::<1>()?[0];
    let dimensions = u32::from_le_bytes(input.array()?);
    let index = match kind {
        0 => boxed(exact::read(&mut input, dimensions)?),
        1 => boxed(crate::ivf_index::image::read(&mut input, dimensions)?),
        2 => boxed(crate::hnsw_index::image::read(&mut input, dimensions)?),
        3 => {
            let default;
            let temporary = if let Some(temporary) = temporary {
                temporary
            } else {
                default = crate::diskann_index::build::DiskANNTemporaryBudget::new(
                    crate::diskann_index::DiskANNIndexOptions::DEFAULT_TEMPORARY_BYTES,
                );
                &default
            };
            boxed(crate::diskann_index::memory::image::read(
                &mut input, dimensions, temporary,
            )?)
        }
        _ => return Err(invalid()),
    };
    input.end()?;
    control.check()?;
    Ok(index)
}

pub(crate) fn boxed<T: VectorIndex + 'static>(
    value: Budgeted<T>,
) -> Budgeted<Box<dyn VectorIndex>> {
    let (value, memory) = value.into_parts();
    // Each concrete decoder admits its struct before construction; boxing transfers that same payload reservation.
    Budgeted::new(Box::new(value), memory)
}

pub(crate) fn header(
    out: &mut ImageOutput<'_>,
    kind: u8,
    dimensions: u32,
) -> StorageBackendResult<()> {
    out.bytes(MAGIC)?;
    out.bytes(&[kind])?;
    out.bytes(&dimensions.to_le_bytes())
}

pub(crate) fn entries<T>(memory: &mut MemoryReservation, count: usize) -> StorageBackendResult<()> {
    memory.grow(
        count
            .checked_mul(size_of::<T>())
            .ok_or(MemoryError::SizeOverflow)?,
    )?;
    Ok(())
}

pub(crate) fn floats(out: &mut ImageOutput<'_>, values: &[f32]) -> StorageBackendResult<()> {
    out.count(values.len())?;
    let mut buffer = [0_u8; 4096];
    let mut values = values.iter();
    while !values.as_slice().is_empty() {
        out.control.check()?;
        let count = coordinate_bytes(&mut values, &mut buffer)?;
        out.bytes(&buffer[..count])?;
    }
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

pub(crate) fn read_floats(
    input: &mut ImageInput<'_>,
    dimensions: u32,
) -> StorageBackendResult<Budgeted<Vec<f32>>> {
    let count = input.count()?;
    if u64::try_from(count).map_err(|_| MemoryError::SizeOverflow)? != u64::from(dimensions) {
        return Err(invalid());
    }
    let mut values = BudgetedVec::new(input.control.memory());
    values.reserve(count)?;
    let mut buffer = [0_u8; 4096];
    while values.len() < count {
        let length = (count - values.len()).min(buffer.len() / 4);
        input.exact(&mut buffer[..length * 4])?;
        for bytes in buffer[..length * 4].chunks_exact(4) {
            let value = f32::from_bits(u32::from_le_bytes(
                bytes.try_into().expect("fixed float width"),
            ));
            if !value.is_finite() {
                return Err(invalid());
            }
            values.push(value)?;
        }
    }
    let (values, memory) = values.into_parts();
    Ok(Budgeted::new(values, memory))
}
