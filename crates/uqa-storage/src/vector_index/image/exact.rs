//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact tensors stream from the retained ordered root without its legacy full-map export.

use super::{
    floats, header, invalid, read_floats, Budgeted, BudgetedVec, ImageInput, ImageOutput,
    MemoryError, StorageBackendResult, StorageReadControl, VectorIndex,
};
use crate::vector_index::{memory_state::MemoryVectorState, MemoryVectorIndex};
use uqa_core::{memory::OwnedMap, DocId};

pub(in crate::vector_index) fn write(
    index: &MemoryVectorIndex,
    writer: &mut dyn std::io::Write,
    control: &StorageReadControl,
) -> StorageBackendResult<u64> {
    let mut out = ImageOutput {
        writer,
        control,
        written: 0,
    };
    header(&mut out, 0, index.dimensions)?;
    out.count(index.state.vectors.len())?;
    out.count(index.count()?)?;
    for (&document, vectors) in &index.state.vectors {
        out.bytes(&document.to_le_bytes())?;
        if vectors.is_empty() {
            return Err(invalid());
        }
        out.count(vectors.len())?;
        for vector in vectors {
            if vector.len() as u64 != u64::from(index.dimensions) {
                return Err(invalid());
            }
            floats(&mut out, vector)?;
        }
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
