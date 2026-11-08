//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One private candidate owns raw and normalized vectors, centroids, assignments and reconstructed lists.

use super::super::IVFIndex;
use super::{
    entries, invalid, read_floats, validation, Budgeted, BudgetedVec, IVFData, IVFState,
    ImageInput, MemoryReservation, StorageBackendResult, StoredVector, VectorKey,
};
use uqa_core::memory::MemoryError;

pub(crate) fn read(input: &mut ImageInput<'_>, dimensions: u32) -> StorageBackendResult<IVFIndex> {
    let mut memory = input.control.memory().reserve(size_of::<IVFIndex>())?;
    let nlist = input.count()?;
    let nprobe = input.count()?;
    let threshold = input.count()?;
    if nlist == 0 || nprobe == 0 || threshold == 0 {
        return Err(invalid());
    }
    let mut index = IVFData::with_params(dimensions, nlist, nprobe, threshold);
    index.state = match input.array::<1>()?[0] {
        0 => IVFState::Untrained,
        1 => IVFState::Trained,
        2 => IVFState::Stale,
        _ => return Err(invalid()),
    };
    index.trained_size = input.count()?;
    index.deletes_since_train = input.count()?;
    let count = input.count()?;
    if count > nlist {
        return Err(invalid());
    }
    let mut centroids = BudgetedVec::new(input.control.memory());
    centroids.reserve(count)?;
    for _ in 0..count {
        let (vector, lease) = read_floats(input, dimensions)?.into_parts();
        memory.absorb(lease);
        centroids.push(vector)?;
    }
    let (centroids, lease) = centroids.into_parts();
    memory.absorb(lease);
    index.centroids = centroids;
    read_vectors(input, &mut index, &mut memory)?;
    restore_lists(&mut index, &mut memory, input.control)?;
    validation::validate(&index, input.control)?;
    Ok(IVFIndex::from_image(Budgeted::new(index, memory)))
}

fn read_vectors(
    input: &mut ImageInput<'_>,
    index: &mut IVFData,
    memory: &mut MemoryReservation,
) -> StorageBackendResult<()> {
    let count = input.count()?;
    entries::<(VectorKey, StoredVector)>(memory, count)?;
    let mut previous = None;
    for _ in 0..count {
        let key = (
            u64::from_le_bytes(input.array()?),
            u32::from_le_bytes(input.array()?),
        );
        if previous.is_some_and(|previous| previous >= key) {
            return Err(invalid());
        }
        let (raw_vector, lease) = read_floats(input, index.dimensions)?.into_parts();
        memory.absorb(lease);
        let (vector, lease) = read_floats(input, index.dimensions)?.into_parts();
        memory.absorb(lease);
        let norm = f32::from_bits(u32::from_le_bytes(input.array()?));
        let centroid = match input.array::<1>()?[0] {
            0 => None,
            1 => Some(input.count()?),
            _ => return Err(invalid()),
        };
        index.vectors.insert(
            key,
            StoredVector {
                key,
                doc_id: key.0,
                vector_ordinal: key.1,
                raw_vector,
                norm,
                vector,
                centroid,
            },
        );
        previous = Some(key);
    }
    Ok(())
}

fn restore_lists(
    index: &mut IVFData,
    memory: &mut MemoryReservation,
    control: &super::StorageReadControl,
) -> StorageBackendResult<()> {
    let count = index.centroids.len();
    let mut counts = BudgetedVec::new(control.memory());
    counts.reserve(count)?;
    for _ in 0..count {
        counts.push(0_usize)?;
    }
    for vector in index.vectors.values() {
        control.check()?;
        if let Some(centroid) = vector.centroid {
            let count = counts.get_mut(centroid).ok_or_else(invalid)?;
            *count = count.checked_add(1).ok_or(MemoryError::SizeOverflow)?;
        }
    }
    let mut lists = BudgetedVec::new(control.memory());
    lists.reserve(count)?;
    for &count in counts.iter() {
        control.check()?;
        let mut list = BudgetedVec::<VectorKey>::new(control.memory());
        list.reserve(count)?;
        let (list, lease) = list.into_parts();
        memory.absorb(lease);
        lists.push(list)?;
    }
    drop(counts);
    for (&key, vector) in &index.vectors {
        control.check()?;
        if let Some(centroid) = vector.centroid {
            // The counted capacity is admitted above; primary map order reproduces the owner's sorted centroid lists.
            lists[centroid].push(key);
        }
    }
    let (lists, lease) = lists.into_parts();
    memory.absorb(lease);
    index.inverted_lists = lists;
    Ok(())
}
