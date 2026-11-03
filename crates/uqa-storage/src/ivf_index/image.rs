//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Images retain evaluated training state and normalization bits without training or normalization replay.

use super::state::{IVFData, IVFState, StoredVector, VectorKey};
use crate::{
    image_io::{ImageInput, ImageOutput},
    read_control::StorageReadControl,
    vector_index::image::{entries, floats, header, invalid, read_floats},
    StorageBackendResult,
};
use std::collections::BTreeMap;
use uqa_core::memory::{Budgeted, BudgetedVec, MemoryReservation};

mod input;
mod validation;

#[cfg(test)]
mod tests;

pub(crate) use input::read;

struct View<'a> {
    index: &'a IVFData,
    vectors: &'a BTreeMap<VectorKey, StoredVector>,
    nprobe: &'a usize,
    state: &'a IVFState,
    centroids: &'a Vec<Vec<f32>>,
    lists: &'a Vec<Vec<VectorKey>>,
    trained_size: &'a usize,
    deletes: &'a usize,
}

impl<'a> View<'a> {
    fn new(index: &'a IVFData, control: &StorageReadControl) -> StorageBackendResult<Self> {
        control.check()?;
        Ok(Self {
            index,
            vectors: &index.vectors,
            nprobe: &index.nprobe,
            state: &index.state,
            centroids: &index.centroids,
            lists: &index.inverted_lists,
            trained_size: &index.trained_size,
            deletes: &index.deletes_since_train,
        })
    }
}

pub(super) fn write(
    index: &IVFData,
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
    for centroid in view.centroids {
        floats(&mut out, centroid)?;
    }
    out.count(view.vectors.len())?;
    for (&(document, ordinal), vector) in view.vectors {
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
