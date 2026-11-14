//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One private candidate owns raw and normalized vectors, centroids, assignments and reconstructed lists.

use super::super::IVFIndex;
use super::{
    entries, invalid, read_floats, validation, Budgeted, BudgetedVec, IVFData, IVFState, Input,
    MemoryReservation, StorageBackendResult, StoredVector, VectorKey,
};
use uqa_core::memory::MemoryError;

pub(crate) async fn read(
    input: &mut Input<'_, '_>,
    dimensions: u32,
) -> StorageBackendResult<IVFIndex> {
    let mut memory = input.control.memory().reserve(size_of::<IVFIndex>())?;
    let nlist = input.source.count().await?;
    let nprobe = input.source.count().await?;
    let threshold = input.source.count().await?;
    if nlist == 0 || nprobe == 0 || threshold == 0 {
        return Err(invalid());
    }
    let mut index = IVFData::with_params(dimensions, nlist, nprobe, threshold);
    index.state = match input.source.array::<1>().await?[0] {
        0 => IVFState::Untrained,
        1 => IVFState::Trained,
        2 => IVFState::Stale,
        _ => return Err(invalid()),
    };
    index.trained_size = input.source.count().await?;
    index.deletes_since_train = input.source.count().await?;
    let count = input.source.count().await?;
    if count > nlist {
        return Err(invalid());
    }
    let mut centroids = BudgetedVec::new(input.control.memory());
    centroids.reserve(count)?;
    for _ in 0..count {
        let (vector, lease) = read_floats(input, dimensions).await?.into_parts();
        memory.absorb(lease);
        centroids.push(vector)?;
    }
    let (centroids, lease) = centroids.into_parts();
    memory.absorb(lease);
    index.centroids = centroids;
    read_vectors(input, &mut index, &mut memory).await?;
    restore_lists(&mut index, &mut memory, input).await?;
    validation::validate_from(&index, input.control, input.source).await?;
    Ok(IVFIndex::from_image(Budgeted::new(index, memory)))
}

async fn read_vectors(
    input: &mut Input<'_, '_>,
    index: &mut IVFData,
    memory: &mut MemoryReservation,
) -> StorageBackendResult<()> {
    let count = input.source.count().await?;
    entries::<(VectorKey, StoredVector)>(memory, count)?;
    let mut previous = None;
    for _ in 0..count {
        let key = (
            u64::from_le_bytes(input.source.array().await?),
            u32::from_le_bytes(input.source.array().await?),
        );
        if previous.is_some_and(|previous| previous >= key) {
            return Err(invalid());
        }
        let (raw_vector, lease) = read_floats(input, index.dimensions).await?.into_parts();
        memory.absorb(lease);
        let (vector, lease) = read_floats(input, index.dimensions).await?.into_parts();
        memory.absorb(lease);
        let norm = f32::from_bits(u32::from_le_bytes(input.source.array().await?));
        let centroid = match input.source.array::<1>().await?[0] {
            0 => None,
            1 => Some(input.source.count().await?),
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

async fn restore_lists(
    index: &mut IVFData,
    memory: &mut MemoryReservation,
    input: &mut Input<'_, '_>,
) -> StorageBackendResult<()> {
    let control = input.control;
    let count = index.centroids.len();
    let mut counts = BudgetedVec::new(control.memory());
    counts.reserve(count)?;
    for _ in 0..count {
        input.source.checkpoint().await?;
        counts.push(0_usize)?;
    }
    for vector in index.vectors.values() {
        input.source.checkpoint().await?;
        control.check()?;
        if let Some(centroid) = vector.centroid {
            let count = counts.get_mut(centroid).ok_or_else(invalid)?;
            *count = count.checked_add(1).ok_or(MemoryError::SizeOverflow)?;
        }
    }
    let mut lists = BudgetedVec::new(control.memory());
    lists.reserve(count)?;
    for &count in counts.iter() {
        input.source.checkpoint().await?;
        control.check()?;
        let mut list = BudgetedVec::<VectorKey>::new(control.memory());
        list.reserve(count)?;
        let (list, lease) = list.into_parts();
        memory.absorb(lease);
        lists.push(list)?;
    }
    drop(counts);
    for (&key, vector) in &index.vectors {
        input.source.checkpoint().await?;
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
