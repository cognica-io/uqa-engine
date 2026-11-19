//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain borrowed term and occurrence positions without copying evaluated text.

use super::{invalid, DocId, MemoryInvertedIndex, StorageBackendResult, StorageReadControl, MAGIC};
use crate::{
    image_io::ImageRead,
    inverted_index::{FieldName, IndexedFieldMetadata, MemoryDocument},
};
use std::ops::Range;
use uqa_core::{memory::BudgetedMapIter, memory::MemoryError, TokenOccurrence};

enum Piece<'a> {
    Borrowed(&'a [u8]),
    Fixed { bytes: [u8; 84], length: usize },
}

impl Piece<'_> {
    fn fixed(input: &[u8]) -> Self {
        let mut bytes = [0; 84];
        bytes[..input.len()].copy_from_slice(input);
        Self::Fixed {
            bytes,
            length: input.len(),
        }
    }

    fn count(value: usize) -> StorageBackendResult<Self> {
        Ok(Self::fixed(
            &u64::try_from(value)
                .map_err(|_| MemoryError::SizeOverflow)?
                .to_le_bytes(),
        ))
    }

    fn bytes(&self) -> &[u8] {
        match self {
            Self::Borrowed(bytes) => bytes,
            Self::Fixed { bytes, length } => &bytes[..*length],
        }
    }
}

enum Stage<'a> {
    Field,
    FieldName(&'a str, IndexedFieldMetadata, Range<usize>),
    FieldMetadata(IndexedFieldMetadata, Range<usize>),
    TermCount(Range<usize>),
    Term(Range<usize>),
    TermBytes(&'a [u8], Range<usize>, &'a [TokenOccurrence]),
    OccurrenceCount(Range<usize>, &'a [TokenOccurrence]),
    Occurrences(Range<usize>, &'a [TokenOccurrence]),
    Complete,
}

pub(super) struct TextImageCursor<'a> {
    index: &'a MemoryInvertedIndex,
    document: &'a MemoryDocument,
    id: DocId,
    fields: BudgetedMapIter<'a, FieldName, IndexedFieldMetadata>,
    stage: Stage<'a>,
    pending: Option<Piece<'a>>,
    position: usize,
    visited: usize,
    control: &'a StorageReadControl,
}

impl<'a> TextImageCursor<'a> {
    pub(super) fn new(
        index: &'a MemoryInvertedIndex,
        id: DocId,
        document: &'a MemoryDocument,
        control: &'a StorageReadControl,
    ) -> StorageBackendResult<Self> {
        let mut header = [0; 16];
        header[..8].copy_from_slice(MAGIC);
        header[8..].copy_from_slice(
            &u64::try_from(document.fields.len())
                .map_err(|_| MemoryError::SizeOverflow)?
                .to_le_bytes(),
        );
        Ok(Self {
            index,
            document,
            id,
            fields: document.fields.iter(),
            stage: Stage::Field,
            pending: Some(Piece::fixed(&header)),
            position: 0,
            visited: 0,
            control,
        })
    }

    fn next_piece(&mut self) -> StorageBackendResult<Option<Piece<'a>>> {
        loop {
            self.control.check()?;
            let piece = match std::mem::replace(&mut self.stage, Stage::Complete) {
                Stage::Field => {
                    let Some((field, metadata)) = self.fields.next() else {
                        if self.visited != self.document.terms.len() {
                            return Err(invalid());
                        }
                        return Ok(None);
                    };
                    let begin = self.document.terms.partition_point(|key| key.0 < *field);
                    let end = self.document.terms.partition_point(|key| key.0 <= *field);
                    self.stage = Stage::FieldName(field, *metadata, begin..end);
                    Piece::count(field.len())?
                }
                Stage::FieldName(field, metadata, terms) => {
                    self.stage = Stage::FieldMetadata(metadata, terms);
                    Piece::Borrowed(field.as_bytes())
                }
                Stage::FieldMetadata(metadata, terms) => {
                    self.stage = Stage::TermCount(terms);
                    Piece::fixed(&metadata.to_bytes()?)
                }
                Stage::TermCount(terms) => {
                    let count = terms.len();
                    self.visited += count;
                    self.stage = Stage::Term(terms);
                    Piece::count(count)?
                }
                Stage::Term(mut terms) => {
                    let Some(position) = terms.next() else {
                        self.stage = Stage::Field;
                        continue;
                    };
                    let key = &self.document.terms[position];
                    let posting = self
                        .index
                        .state
                        .index
                        .get(key)
                        .and_then(|postings| postings.get(&self.id))
                        .ok_or_else(invalid)?;
                    self.stage = Stage::TermBytes(key.1.as_bytes(), terms, &posting.occurrences);
                    Piece::count(key.1.as_bytes().len())?
                }
                Stage::TermBytes(bytes, terms, occurrences) => {
                    self.stage = Stage::OccurrenceCount(terms, occurrences);
                    Piece::Borrowed(bytes)
                }
                Stage::OccurrenceCount(terms, occurrences) => {
                    self.stage = Stage::Occurrences(terms, occurrences);
                    Piece::count(occurrences.len())?
                }
                Stage::Occurrences(terms, occurrences) => {
                    let Some((occurrence, remaining)) = occurrences.split_first() else {
                        self.stage = Stage::Term(terms);
                        continue;
                    };
                    occurrence.validate().map_err(|_| invalid())?;
                    let mut bytes = [0; 41];
                    bytes[..4].copy_from_slice(&occurrence.position.to_le_bytes());
                    bytes[4..8].copy_from_slice(&occurrence.position_length.to_le_bytes());
                    bytes[8] = u8::from(occurrence.offsets.is_some());
                    let length = if let Some(offsets) = occurrence.offsets {
                        for (slot, number) in bytes[9..].chunks_exact_mut(8).zip([
                            offsets.start_utf8,
                            offsets.end_utf8,
                            offsets.start_utf16,
                            offsets.end_utf16,
                        ]) {
                            slot.copy_from_slice(&number.to_le_bytes());
                        }
                        41
                    } else {
                        9
                    };
                    self.stage = Stage::Occurrences(terms, remaining);
                    Piece::fixed(&bytes[..length])
                }
                Stage::Complete => return Ok(None),
            };
            return Ok(Some(piece));
        }
    }
}

impl ImageRead for TextImageCursor<'_> {
    fn read(&mut self, output: &mut [u8]) -> StorageBackendResult<usize> {
        self.control.check()?;
        let limit = output.len().min(16 * 1024);
        let mut written = 0;
        while written < limit {
            if self.pending.is_none() {
                self.pending = self.next_piece()?;
                self.position = 0;
            }
            let Some(piece) = &self.pending else {
                break;
            };
            let bytes = &piece.bytes()[self.position..];
            let count = bytes.len().min(limit - written);
            output[written..written + count].copy_from_slice(&bytes[..count]);
            written += count;
            self.position += count;
            if count == bytes.len() {
                self.pending = None;
            }
        }
        self.control.check()?;
        Ok(written)
    }
}
