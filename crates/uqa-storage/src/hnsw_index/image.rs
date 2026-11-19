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
    image_io::{ImageOutput, ImageRead, ImageReader},
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
    let mut cursor = stream::HNSWImageCursor::new(index, control)?;
    while !cursor.prepare(128)?.ready {}
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

async fn flag(input: &mut Input<'_, '_>) -> StorageBackendResult<bool> {
    match input.source.array::<1>().await?[0] {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(invalid()),
    }
}
