//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete HNSW images retain topology, exact normalization and pending persistence without graph replay.

use super::{
    types::{active_key, HNSWIndex, HNSWNode, HNSWVector},
    MAX_HNSW_LEVEL,
};
use crate::{
    image_io::{write_from_cursor, ImageRead, ImageReader},
    read_control::StorageReadControl,
    vector_index::image::{invalid, read_floats, Input},
    StorageBackendResult,
};
use uqa_core::memory::{Budgeted, BudgetedVec};

mod input;
mod stream;
#[cfg(test)]
mod tests;
mod validation;
pub(crate) use input::read;

pub(super) fn reader<'a>(
    index: &'a HNSWIndex,
    control: &'a StorageReadControl,
) -> StorageBackendResult<ImageReader<'a>> {
    ImageReader::new(stream::HNSWImageCursor::new(index, control)?, control)
}

pub(super) fn write(
    index: &HNSWIndex,
    writer: &mut dyn std::io::Write,
    control: &StorageReadControl,
) -> StorageBackendResult<u64> {
    let cursor = stream::HNSWImageCursor::new(index, control)?;
    write_from_cursor(cursor, writer, control, None)
}

async fn flag(input: &mut Input<'_, '_>) -> StorageBackendResult<bool> {
    match input.source.array::<1>().await?[0] {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(invalid()),
    }
}
