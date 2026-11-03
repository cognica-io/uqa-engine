//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluated text documents retain graph occurrences and historical field revisions.

use std::io::{Read, Write};

use super::{
    DocId, MemoryInvertedIndex, StagedMemoryDocument, StorageBackendError, StorageBackendResult,
};
use crate::read_control::StorageReadControl;
use uqa_core::memory::Budgeted;

mod input;
mod io;
mod output;
#[cfg(test)]
pub(super) mod tests;

const MAGIC: &[u8; 8] = b"UQATXT01";

fn invalid() -> StorageBackendError {
    StorageBackendError::Other("invalid evaluated text document image".into())
}

pub(super) fn write(
    index: &MemoryInvertedIndex,
    id: DocId,
    output: &mut dyn Write,
    control: &StorageReadControl,
) -> StorageBackendResult<Option<u64>> {
    index.check_retained_read()?;
    control.check()?;
    let Some(document) = index.state.document(id)? else {
        return Ok(None);
    };
    crate::document_store::image::with_retained_control(
        output,
        index.read_control.as_ref(),
        |output| output::write(index, id, document, output, control).map(Some),
    )
}

pub(super) fn read(
    id: DocId,
    input: &mut dyn Read,
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<StagedMemoryDocument>> {
    input::read(id, input, control)
}
