//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact tensors stream from the retained ordered root without its legacy full-map export.

use super::{
    invalid, read_floats, Budgeted, BudgetedVec, ImageInput, ImageOutput, MemoryError,
    StorageBackendResult, StorageReadControl, VectorIndex,
};
use crate::{
    image_io::{ImageRead, ImageReader},
    vector_index::{memory_state::MemoryVectorState, MemoryVectorIndex},
};
use uqa_core::{memory::OwnedMap, DocId};

mod stream;
#[cfg(test)]
mod tests;

pub(in crate::vector_index) fn reader<'a>(
    index: &'a MemoryVectorIndex,
    control: &'a StorageReadControl,
) -> StorageBackendResult<ImageReader<'a>> {
    ImageReader::new(stream::ExactImageCursor::new(index, control)?, control)
}

pub(in crate::vector_index) fn write(
    index: &MemoryVectorIndex,
    writer: &mut dyn std::io::Write,
    control: &StorageReadControl,
) -> StorageBackendResult<u64> {
    let mut cursor = stream::ExactImageCursor::new(index, control)?;
    let mut out = ImageOutput {
        writer,
        control,
        written: 0,
    };
    let mut buffer = [0; 4096];
    loop {
        let count = cursor.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        out.bytes(&buffer[..count])?;
    }
    control.check()?;
    Ok(out.written)
}

pub(super) fn read(
    input: &mut ImageInput<'_>,
    dimensions: u32,
) -> StorageBackendResult<Budgeted<MemoryVectorIndex>> {
    let mut memory = input
        .control
        .memory()
        .reserve(size_of::<MemoryVectorIndex>() + size_of::<MemoryVectorState>())?;
    let count = input.count()?;
    let expected_vectors = input.count()?;
    memory.grow(
        count
            .checked_mul(OwnedMap::<DocId, Vec<Vec<f32>>>::entry_bytes())
            .ok_or(MemoryError::SizeOverflow)?,
    )?;
    let mut index = MemoryVectorIndex::new(dimensions);
    let state = std::sync::Arc::get_mut(&mut index.state).expect("private image root is unique");
    let mut previous = None;
    for _ in 0..count {
        let document = u64::from_le_bytes(input.array()?);
        if previous.is_some_and(|previous| previous >= document) {
            return Err(invalid());
        }
        let count = input.count()?;
        if count == 0 {
            return Err(invalid());
        }
        let mut tensor = BudgetedVec::new(input.control.memory());
        tensor.reserve(count)?;
        for _ in 0..count {
            let (vector, lease) = read_floats(input, dimensions)?.into_parts();
            memory.absorb(lease);
            tensor.push(vector)?;
        }
        let (tensor, lease) = tensor.into_parts();
        memory.absorb(lease);
        state.replace(document, tensor);
        previous = Some(document);
    }
    if index.count()? != expected_vectors {
        return Err(invalid());
    }
    Ok(Budgeted::new(index, memory))
}
