//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Admission and complete validation precede publication of immutable native paths.

use super::{
    invalid, CapturedPath, CapturedPathIndex, MemoryPaths, PathIndex, PathStorage,
    StorageBackendResult, StorageReadControl, MAGIC,
};
use std::{cmp::Ordering, io::Read, sync::Arc};
use uqa_core::memory::{BudgetedVec, MemoryReservation};
use uqa_storage::image_io::ImageInput;

pub(super) fn read(
    reader: &mut dyn Read,
    control: &StorageReadControl,
) -> StorageBackendResult<PathIndex> {
    let mut input = ImageInput { reader, control };
    if &input.array::<8>()? != MAGIC {
        return Err(invalid());
    }
    let mut memory = control.memory().reserve(size_of::<MemoryPaths>())?;
    let count = input.count()?;
    let mut paths: BudgetedVec<CapturedPath> = BudgetedVec::new(control.memory());
    paths.reserve(count)?;
    for _ in 0..count {
        let sequence = sequence(&mut input, &mut memory)?;
        if let Some(previous) = paths.last() {
            if compare(&previous.sequence, &sequence, control)? != Ordering::Less {
                return Err(invalid());
            }
        }
        let pairs = pairs(&mut input, &mut memory)?;
        paths.push(CapturedPath { sequence, pairs })?;
    }
    input.end()?;
    let (paths, reservation) = paths.into_parts();
    memory.absorb(reservation);
    control.check()?;
    Ok(PathIndex {
        storage: PathStorage::Memory(Arc::new(MemoryPaths::from_admitted(
            CapturedPathIndex { paths },
            memory,
        ))),
    })
}

fn sequence(
    input: &mut ImageInput<'_>,
    memory: &mut MemoryReservation,
) -> StorageBackendResult<Vec<String>> {
    let count = input.count()?;
    let mut sequence = BudgetedVec::new(input.control.memory());
    sequence.reserve(count)?;
    for _ in 0..count {
        let (label, reservation) = input.text()?.into_parts();
        memory.absorb(reservation);
        sequence.push(label)?;
    }
    let (sequence, reservation) = sequence.into_parts();
    memory.absorb(reservation);
    Ok(sequence)
}

fn pairs(
    input: &mut ImageInput<'_>,
    memory: &mut MemoryReservation,
) -> StorageBackendResult<Vec<(u64, u64)>> {
    let count = input.count()?;
    let mut pairs = BudgetedVec::new(input.control.memory());
    pairs.reserve(count)?;
    for _ in 0..count {
        let pair = (
            u64::from_le_bytes(input.array()?),
            u64::from_le_bytes(input.array()?),
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

fn compare(
    left: &[String],
    right: &[String],
    control: &StorageReadControl,
) -> StorageBackendResult<Ordering> {
    for (left, right) in left.iter().zip(right) {
        control.check()?;
        for (left, right) in left
            .as_bytes()
            .chunks(4096)
            .zip(right.as_bytes().chunks(4096))
        {
            control.check()?;
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
    control.check()?;
    Ok(left.len().cmp(&right.len()))
}
