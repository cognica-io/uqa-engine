//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The native vector grammar shares its input and publication progress across callers.

use super::{
    exact, invalid, StorageBackendError, StorageBackendResult, StorageReadControl, VectorIndex,
    MAGIC,
};
use crate::diskann_index::build::DiskANNTemporaryBudget;
use uqa_core::{
    image_io::{decode_image, AsyncImageInput, ImageDecoder},
    memory::{Budgeted, BudgetedVec, MemoryError},
};

pub(crate) struct Input<'a, 'b> {
    pub(crate) source: &'a mut AsyncImageInput<'b>,
    pub(crate) control: &'a StorageReadControl,
}

/// Retain native vector fields, validation, spill/compaction and physical `DiskANN` preparation across current byte/poll windows. Failure or abandonment retires the unpublished owner and its original allocation leases. Collection input owns field-name admission and source registration through the same channel. Native allocation, tree comparisons and individual filesystem calls remain atomic operations; the windows do not assert elapsed-time bounds. A supplied temporary budget belongs to future `DiskANN` mutations.
pub fn vector_image_input(
    control: &StorageReadControl,
    temporary: Option<&DiskANNTemporaryBudget>,
) -> StorageBackendResult<ImageDecoder<Box<dyn VectorIndex>, StorageBackendError>> {
    let original = control.clone();
    let temporary = temporary.cloned();
    Ok(ImageDecoder::new(
        control.memory(),
        control.cancellation(),
        move |mut reader| async move {
            let mut source = reader.input();
            let mut input = Input {
                source: &mut source,
                control: &original,
            };
            read_from(&mut input, temporary.as_ref()).await
        },
    )?)
}

pub(super) fn read(
    reader: &mut dyn std::io::Read,
    control: &StorageReadControl,
    temporary: Option<&DiskANNTemporaryBudget>,
) -> StorageBackendResult<Box<dyn VectorIndex>> {
    decode_image(
        reader,
        control.memory(),
        control.cancellation(),
        async |mut source| {
            let mut input = Input {
                source: &mut source,
                control,
            };
            read_from(&mut input, temporary).await
        },
    )
}

pub(crate) async fn read_from(
    input: &mut Input<'_, '_>,
    temporary: Option<&DiskANNTemporaryBudget>,
) -> StorageBackendResult<Box<dyn VectorIndex>> {
    input.control.check()?;
    if &input.source.array::<8>().await? != MAGIC {
        return Err(invalid());
    }
    let kind = input.source.array::<1>().await?[0];
    let dimensions = u32::from_le_bytes(input.source.array().await?);
    let index: Box<dyn VectorIndex> = match kind {
        0 => Box::new(exact::read(input, dimensions).await?),
        1 => Box::new(crate::ivf_index::image::read(input, dimensions).await?),
        2 => Box::new(crate::hnsw_index::image::read(input, dimensions).await?),
        3 => {
            let default;
            let temporary = if let Some(temporary) = temporary {
                temporary
            } else {
                default = DiskANNTemporaryBudget::new(
                    crate::diskann_index::DiskANNIndexOptions::DEFAULT_TEMPORARY_BYTES,
                );
                &default
            };
            Box::new(crate::diskann_index::memory::image::read(input, dimensions, temporary).await?)
        }
        _ => return Err(invalid()),
    };
    input.source.end().await?;
    input.control.check()?;
    Ok(index)
}

pub(crate) async fn read_floats(
    input: &mut Input<'_, '_>,
    dimensions: u32,
) -> StorageBackendResult<Budgeted<Vec<f32>>> {
    let count = input.source.count().await?;
    if u64::try_from(count).map_err(|_| MemoryError::SizeOverflow)? != u64::from(dimensions) {
        return Err(invalid());
    }
    let mut values = BudgetedVec::new(input.control.memory());
    values.reserve(count)?;
    let mut buffer = [0_u8; 4096];
    while values.len() < count {
        let length = (count - values.len()).min(buffer.len() / 4);
        input.source.exact(&mut buffer[..length * 4]).await?;
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
