//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Admission and complete validation precede publication of immutable native paths.

use super::{
    invalid, CapturedPath, CapturedPathIndex, MemoryPaths, PathIndex, PathStorage,
    StorageBackendResult, MAGIC,
};
use std::{cmp::Ordering, sync::Arc};
use uqa_core::image_io::AsyncImageInput;
use uqa_core::memory::{BudgetedVec, MemoryReservation};

pub(super) async fn read_from(input: &mut AsyncImageInput<'_>) -> StorageBackendResult<PathIndex> {
    if &input.array::<8>().await? != MAGIC {
        return Err(invalid());
    }
    let mut memory = input.memory.reserve(size_of::<MemoryPaths>())?;
    let count = input.count().await?;
    let mut paths: BudgetedVec<CapturedPath> = BudgetedVec::new(input.memory);
    paths.reserve(count)?;
    for _ in 0..count {
        let sequence = sequence(input, &mut memory).await?;
        if let Some(previous) = paths.last() {
            if compare(&previous.sequence, &sequence, input).await? != Ordering::Less {
                return Err(invalid());
            }
        }
        let pairs = pairs(input, &mut memory).await?;
        paths.push(CapturedPath { sequence, pairs })?;
    }
    input.end().await?;
    let (paths, reservation) = paths.into_parts();
    memory.absorb(reservation);
    input.checkpoint().await?;
    Ok(PathIndex {
        storage: PathStorage::Memory(Arc::new(MemoryPaths::from_admitted(
            CapturedPathIndex { paths },
            memory,
        ))),
    })
}

async fn sequence(
    input: &mut AsyncImageInput<'_>,
    memory: &mut MemoryReservation,
) -> StorageBackendResult<Vec<String>> {
    let count = input.count().await?;
    let mut sequence = BudgetedVec::new(input.memory);
    sequence.reserve(count)?;
    for _ in 0..count {
        let (label, reservation) = input.text().await?.into_parts();
        memory.absorb(reservation);
        sequence.push(label)?;
    }
    let (sequence, reservation) = sequence.into_parts();
    memory.absorb(reservation);
    Ok(sequence)
}

async fn pairs(
    input: &mut AsyncImageInput<'_>,
    memory: &mut MemoryReservation,
) -> StorageBackendResult<Vec<(u64, u64)>> {
    let count = input.count().await?;
    let mut pairs = BudgetedVec::new(input.memory);
    pairs.reserve(count)?;
    for _ in 0..count {
        let pair = (
            u64::from_le_bytes(input.array().await?),
            u64::from_le_bytes(input.array().await?),
        );
        if pairs.last().is_some_and(|previous| *previous >= pair) {
            return Err(invalid());
        }
        pairs.push(pair)?;
    }
    let (pairs, reservation) = pairs.into_parts();
    memory.absorb(reservation);
    Ok(pairs)
}

async fn compare(
    left: &[String],
    right: &[String],
    input: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<Ordering> {
    for (left, right) in left.iter().zip(right) {
        input.checkpoint().await?;
        for (left, right) in left
            .as_bytes()
            .chunks(4096)
            .zip(right.as_bytes().chunks(4096))
        {
            input.checkpoint().await?;
            let order = left.cmp(right);
            if order != Ordering::Equal {
                return Ok(order);
            }
        }
        let order = left.len().cmp(&right.len());
        if order != Ordering::Equal {
            return Ok(order);
        }
    }
    input.checkpoint().await?;
    Ok(left.len().cmp(&right.len()))
}
