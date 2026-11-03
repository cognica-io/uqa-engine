//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Images retain evaluated training state and normalization bits without training or normalization replay.

use super::state::{IVFIndex, IVFState, StoredVector, VectorKey};
use crate::{
    image_io::{ImageInput, ImageOutput},
    read_control::StorageReadControl,
    vector_index::image::{entries, floats, header, invalid, read_floats},
    StorageBackendResult,
};
use parking_lot::{Mutex, MutexGuard};
use std::{collections::BTreeMap, time::Duration};
use uqa_core::memory::{Budgeted, BudgetedVec, MemoryReservation};

mod input;
mod validation;

#[cfg(test)]
mod tests;

pub(crate) use input::read;

struct View<'a> {
    index: &'a IVFIndex,
    vectors: MutexGuard<'a, BTreeMap<VectorKey, StoredVector>>,
    nprobe: MutexGuard<'a, usize>,
    state: MutexGuard<'a, IVFState>,
    centroids: MutexGuard<'a, Vec<Vec<f32>>>,
    lists: MutexGuard<'a, Vec<Vec<VectorKey>>>,
    trained_size: MutexGuard<'a, usize>,
    deletes: MutexGuard<'a, usize>,
}

fn lock<'a, T>(
    value: &'a Mutex<T>,
    control: &StorageReadControl,
) -> StorageBackendResult<MutexGuard<'a, T>> {
    loop {
        control.check()?;
        if let Some(guard) = value.try_lock_for(Duration::from_millis(10)) {
            return Ok(guard);
        }
    }
}

impl<'a> View<'a> {
    fn new(index: &'a IVFIndex, control: &StorageReadControl) -> StorageBackendResult<Self> {
        // Training publishes all dependent fields under the vectors lock. The selected source must be detached from the live Engine before repository output begins.
        let vectors = lock(&index.vectors, control)?;
        Ok(Self {
            index,
            vectors,
            nprobe: lock(&index.nprobe, control)?,
            state: lock(&index.state, control)?,
            centroids: lock(&index.centroids, control)?,
            lists: lock(&index.inverted_lists, control)?,
            trained_size: lock(&index.trained_size, control)?,
            deletes: lock(&index.deletes_since_train, control)?,
        })
    }
}

pub(super) fn write(
    index: &IVFIndex,
    writer: &mut dyn std::io::Write,
    control: &StorageReadControl,
) -> StorageBackendResult<u64> {
    let view = View::new(index, control)?;
    validation::validate(&view, control)?;
    let mut out = ImageOutput {
        writer,
        control,
        written: 0,
    };
    header(&mut out, 1, index.dimensions)?;
    for count in [index.nlist, *view.nprobe, index.train_threshold] {
        out.count(count)?;
    }
    out.bytes(&[match *view.state {
        IVFState::Untrained => 0,
        IVFState::Trained => 1,
        IVFState::Stale => 2,
    }])?;
    out.count(*view.trained_size)?;
    out.count(*view.deletes)?;
    out.count(view.centroids.len())?;
    for centroid in view.centroids.iter() {
        floats(&mut out, centroid)?;
    }
    out.count(view.vectors.len())?;
    for (&(document, ordinal), vector) in view.vectors.iter() {
        out.bytes(&document.to_le_bytes())?;
        out.bytes(&ordinal.to_le_bytes())?;
        floats(&mut out, &vector.raw_vector)?;
        floats(&mut out, &vector.vector)?;
        out.bytes(&vector.norm.to_bits().to_le_bytes())?;
        match vector.centroid {
            Some(centroid) => {
                out.bytes(&[1])?;
                out.count(centroid)?;
            }
            None => out.bytes(&[0])?,
        }
    }
    control.check()?;
    Ok(out.written)
}
