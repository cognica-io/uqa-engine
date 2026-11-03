//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow existing reverse keys and occurrences instead of collecting a second corpus.

use super::{
    invalid, io::Output, DocId, MemoryInvertedIndex, StorageBackendResult, StorageReadControl,
    MAGIC,
};
use crate::inverted_index::MemoryDocument;
use uqa_core::TokenOccurrence;

pub(super) fn write(
    index: &MemoryInvertedIndex,
    id: DocId,
    document: &MemoryDocument,
    writer: &mut dyn std::io::Write,
    control: &StorageReadControl,
) -> StorageBackendResult<u64> {
    let mut output = Output {
        writer,
        control,
        written: 0,
    };
    output.bytes(MAGIC)?;
    output.count(document.fields.len())?;
    let mut visited = 0;
    for (field, metadata) in &document.fields {
        control.check()?;
        output.sized(field.as_bytes())?;
        output.bytes(&metadata.to_bytes()?)?;
        let begin = document.terms.partition_point(|key| key.0 < *field);
        let end = document.terms.partition_point(|key| key.0 <= *field);
        output.count(end - begin)?;
        for key in &document.terms[begin..end] {
            control.check()?;
            let posting = index
                .state
                .index
                .get(key)
                .and_then(|postings| postings.get(&id))
                .ok_or_else(invalid)?;
            output.sized(key.1.as_bytes())?;
            output.count(posting.occurrences.len())?;
            for occurrence in &posting.occurrences {
                write_occurrence(&mut output, occurrence)?;
            }
        }
        visited += end - begin;
    }
    if visited != document.terms.len() {
        return Err(invalid());
    }
    control.check()?;
    Ok(output.written)
}

fn write_occurrence(
    output: &mut Output<'_>,
    occurrence: &TokenOccurrence,
) -> StorageBackendResult<()> {
    occurrence.validate().map_err(|_| invalid())?;
    output.bytes(&occurrence.position.to_le_bytes())?;
    output.bytes(&occurrence.position_length.to_le_bytes())?;
    output.bytes(&[u8::from(occurrence.offsets.is_some())])?;
    if let Some(offsets) = occurrence.offsets {
        for number in [
            offsets.start_utf8,
            offsets.end_utf8,
            offsets.start_utf16,
            offsets.end_utf16,
        ] {
            output.bytes(&number.to_le_bytes())?;
        }
    }
    Ok(())
}
