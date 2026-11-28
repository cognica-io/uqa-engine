//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Images retain evaluated training state and normalization bits without training or normalization replay.

use super::state::{IVFData, IVFState, StoredVector, VectorKey};
use crate::{
    image_io::{write_from_cursor, ImageRead, ImageReader},
    read_control::StorageReadControl,
    vector_index::image::{entries, invalid, read_floats, Input},
    StorageBackendResult,
};
use uqa_core::memory::{Budgeted, BudgetedVec, MemoryReservation};

mod input;
mod stream;
mod validation;

#[cfg(test)]
mod tests;

pub(crate) use input::read;

pub(super) fn reader<'a>(
    index: &'a IVFData,
    control: &'a StorageReadControl,
) -> StorageBackendResult<ImageReader<'a>> {
    ImageReader::new(stream::IVFImageCursor::new(index, control)?, control)
}

pub(super) fn write(
    index: &IVFData,
    writer: &mut dyn std::io::Write,
    control: &StorageReadControl,
) -> StorageBackendResult<u64> {
    let cursor = stream::IVFImageCursor::new(index, control)?;
    write_from_cursor(cursor, writer, control, None)
}
