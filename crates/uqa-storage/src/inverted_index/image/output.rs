//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow existing reverse keys and occurrences instead of collecting a second corpus.

use super::{
    stream::TextImageCursor, DocId, MemoryInvertedIndex, StorageBackendResult, StorageReadControl,
};
use crate::{
    image_io::{ImageOutput, ImageRead},
    inverted_index::MemoryDocument,
};

pub(super) fn write(
    index: &MemoryInvertedIndex,
    id: DocId,
    document: &MemoryDocument,
    writer: &mut dyn std::io::Write,
    control: &StorageReadControl,
) -> StorageBackendResult<u64> {
    let mut reader = TextImageCursor::new(index, id, document, control)?;
    let mut output = ImageOutput {
        writer,
        control,
        written: 0,
    };
    let mut bytes = [0; 4096];
    loop {
        let count = reader.read(&mut bytes)?;
        if count == 0 {
            return Ok(output.written);
        }
        output.bytes(&bytes[..count])?;
    }
}
