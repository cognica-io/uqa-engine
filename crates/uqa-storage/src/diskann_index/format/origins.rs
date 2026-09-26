//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use sha2::{Digest, Sha256};
use uqa_core::{memory::BudgetedVec, DocId};

use super::{invalid, record, DiskANNCanonicalOrigin, DiskANNGeneration, CANONICAL_ORIGIN_BYTES};
use crate::{read_control::StorageReadControl, StorageBackendResult};

#[cfg(test)]
mod tests;

const MAGIC: [u8; 8] = *b"UQADNOR\0";
const METADATA_BYTES: usize = 32;
pub const ORIGIN_ENTRY_BYTES: usize = 8 + CANONICAL_ORIGIN_BYTES;
pub const ORIGIN_BATCH_DOCUMENTS: usize = 64;

/// Complete ordered origin artifact metadata. This is an integrity descriptor, not authority to publish or retire changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskANNOriginSummary {
    documents: u64,
    digest: [u8; 32],
}

impl DiskANNOriginSummary {
    pub const ENCODED_BYTES: usize = 40;

    pub(crate) fn new(documents: u64, digest: [u8; 32]) -> StorageBackendResult<Self> {
        if documents == 0 && digest != <[u8; 32]>::from(Sha256::digest([])) {
            return Err(invalid("empty origin stream requires the empty digest"));
        }
        Ok(Self { documents, digest })
    }

    pub fn documents(self) -> u64 {
        self.documents
    }
    pub fn digest(self) -> [u8; 32] {
        self.digest
    }
}

/// One complete selected tensor, including an explicit empty replacement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskANNOriginEntry {
    document: DocId,
    origin: DiskANNCanonicalOrigin,
}

impl DiskANNOriginEntry {
    pub fn new(document: DocId, origin: DiskANNCanonicalOrigin) -> Self {
        Self { document, origin }
    }
    pub fn document(self) -> DocId {
        self.document
    }
    pub fn origin(self) -> DiskANNCanonicalOrigin {
        self.origin
    }

    pub(crate) fn encode(self) -> [u8; ORIGIN_ENTRY_BYTES] {
        let mut bytes = [0; ORIGIN_ENTRY_BYTES];
        bytes[..8].copy_from_slice(&self.document.to_le_bytes());
        bytes[8..].copy_from_slice(&self.origin.encode());
        bytes
    }

    pub(crate) fn decode(bytes: &[u8], dimensions: u32) -> StorageBackendResult<Self> {
        if bytes.len() != ORIGIN_ENTRY_BYTES {
            return Err(invalid("origin entry length differs"));
        }
        Ok(Self::new(
            record::u64_at(bytes, 0)?,
            DiskANNCanonicalOrigin::decode(&bytes[8..], dimensions)?,
        ))
    }
}

/// Fixed-capacity batches use dense document positions while preserving sparse document IDs. Point lookup needs no resident directory.
#[derive(Debug, Clone, Copy)]
pub struct DiskANNOriginLayout {
    generation: DiskANNGeneration,
    dimensions: u32,
    documents: u64,
}

pub struct DiskANNOriginBatch<'a> {
    dimensions: u32,
    entries: &'a [u8],
}

impl DiskANNOriginBatch<'_> {
    pub fn len(&self) -> usize {
        self.entries.len() / ORIGIN_ENTRY_BYTES
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn bytes(&self) -> &[u8] {
        self.entries
    }
    pub fn entry(&self, index: usize) -> Option<DiskANNOriginEntry> {
        let start = index.checked_mul(ORIGIN_ENTRY_BYTES)?;
        let end = start.checked_add(ORIGIN_ENTRY_BYTES)?;
        Some(
            DiskANNOriginEntry::decode(self.entries.get(start..end)?, self.dimensions)
                .expect("validated origin entry"),
        )
    }
}

impl DiskANNOriginLayout {
    pub const MAX_ENCODED_BYTES: usize =
        record::HEADER_BYTES + METADATA_BYTES + ORIGIN_BATCH_DOCUMENTS * ORIGIN_ENTRY_BYTES;

    pub fn new(
        generation: DiskANNGeneration,
        dimensions: u32,
        documents: u64,
    ) -> StorageBackendResult<Self> {
        if dimensions == 0 {
            return Err(invalid("origin dimensions must be positive"));
        }
        Ok(Self {
            generation,
            dimensions,
            documents,
        })
    }

    pub fn encode(
        self,
        first: u64,
        entries: &[DiskANNOriginEntry],
        control: &StorageReadControl,
    ) -> StorageBackendResult<BudgetedVec<u8>> {
        self.check_extent(first, entries.len())?;
        let mut bytes = record::begin(
            MAGIC,
            self.generation,
            self.encoded_bytes(first)? - record::HEADER_BYTES,
            control,
        )?;
        bytes.extend_from_slice(&self.dimensions.to_le_bytes())?;
        bytes.extend_from_slice(&(ORIGIN_BATCH_DOCUMENTS as u32).to_le_bytes())?;
        for value in [self.documents, first, entries.len() as u64] {
            bytes.extend_from_slice(&value.to_le_bytes())?;
        }
        let mut previous = None;
        for &entry in entries {
            control.check()?;
            let encoded = entry.encode();
            DiskANNOriginEntry::decode(&encoded, self.dimensions)?;
            ordered(&mut previous, entry)?;
            bytes.extend_from_slice(&encoded)?;
        }
        record::finish(bytes, control)
    }

    pub fn decode<'a>(
        self,
        first: u64,
        bytes: &'a [u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNOriginBatch<'a>> {
        let body = record::open(MAGIC, self.generation, bytes, control)?;
        if body.len() < METADATA_BYTES
            || !(body.len() - METADATA_BYTES).is_multiple_of(ORIGIN_ENTRY_BYTES)
        {
            return Err(invalid("origin batch length differs"));
        }
        let entries = &body[METADATA_BYTES..];
        let count = entries.len() / ORIGIN_ENTRY_BYTES;
        self.check_extent(first, count)?;
        if record::u32_at(body, 0)? != self.dimensions
            || record::u32_at(body, 4)? != ORIGIN_BATCH_DOCUMENTS as u32
            || record::u64_at(body, 8)? != self.documents
            || record::u64_at(body, 16)? != first
            || record::u64_at(body, 24)? != count as u64
        {
            return Err(invalid("origin batch shape or address differs"));
        }
        let mut previous = None;
        for bytes in entries.chunks_exact(ORIGIN_ENTRY_BYTES) {
            control.check()?;
            ordered(
                &mut previous,
                DiskANNOriginEntry::decode(bytes, self.dimensions)?,
            )?;
        }
        control.check()?;
        Ok(DiskANNOriginBatch {
            dimensions: self.dimensions,
            entries,
        })
    }

    fn check_extent(self, first: u64, count: usize) -> StorageBackendResult<()> {
        if first >= self.documents
            || !first.is_multiple_of(ORIGIN_BATCH_DOCUMENTS as u64)
            || count as u64 != (self.documents - first).min(ORIGIN_BATCH_DOCUMENTS as u64)
        {
            return Err(invalid(
                "origin batch does not cover its exact fixed extent",
            ));
        }
        Ok(())
    }

    pub fn encoded_bytes(self, first: u64) -> StorageBackendResult<usize> {
        let count = self
            .documents
            .saturating_sub(first)
            .min(ORIGIN_BATCH_DOCUMENTS as u64) as usize;
        self.check_extent(first, count)?;
        Ok(record::HEADER_BYTES + METADATA_BYTES + count * ORIGIN_ENTRY_BYTES)
    }
}

fn ordered(previous: &mut Option<DocId>, entry: DiskANNOriginEntry) -> StorageBackendResult<()> {
    if previous.is_some_and(|last| last >= entry.document) {
        return Err(invalid("origin documents must be unique and ordered"));
    }
    *previous = Some(entry.document);
    Ok(())
}
