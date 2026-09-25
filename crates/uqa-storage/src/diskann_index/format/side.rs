//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::{memory::BudgetedVec, DocId};

use super::{field, invalid, record, DiskANNGeneration, DiskANNVectorVersion};
use crate::diskann_index::{metric::norms, ExactVectorReason};
use crate::mvcc::{DatabaseId, StorageTransactionId};
use crate::{read_control::StorageReadControl, StorageBackendResult};

const MAGIC: [u8; 8] = *b"UQADNSD\0";
const METADATA_BYTES: usize = 32;
const ENTRY_BYTES: usize = 48;
const CLASSIFICATION_REVISION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskANNSideEntry {
    dimensions: u32,
    doc: DocId,
    ordinal: u32,
    version: DiskANNVectorVersion,
    reason: ExactVectorReason,
}

impl DiskANNSideEntry {
    pub fn from_raw(
        dimensions: u32,
        doc: DocId,
        ordinal: u32,
        version: DiskANNVectorVersion,
        raw: &[f32],
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        let (norm, _) = norms(dimensions, raw, control)?;
        let reason = if norm == 0.0 {
            ExactVectorReason::ZeroNorm
        } else if !norm.is_finite() {
            ExactVectorReason::NonFiniteNorm
        } else {
            return Err(invalid(
                "navigable vector does not belong in the numeric side stream",
            ));
        };
        control.check()?;
        Ok(Self {
            dimensions,
            doc,
            ordinal,
            version,
            reason,
        })
    }
    pub fn doc_id(self) -> DocId {
        self.doc
    }
    pub fn ordinal(self) -> u32 {
        self.ordinal
    }
    pub fn version(self) -> DiskANNVectorVersion {
        self.version
    }
    pub fn reason(self) -> ExactVectorReason {
        self.reason
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskANNSideLayout {
    generation: DiskANNGeneration,
    dimensions: u32,
    count: u64,
}

#[derive(Debug)]
pub struct DiskANNSideBatch<'a> {
    dimensions: u32,
    start: u64,
    entries: &'a [u8],
}

impl DiskANNSideBatch<'_> {
    pub fn first_record(&self) -> u64 {
        self.start
    }
    pub fn record_count(&self) -> usize {
        self.entries.len() / ENTRY_BYTES
    }
    pub fn bytes(&self) -> &[u8] {
        self.entries
    }
    pub fn entry(&self, index: usize) -> Option<DiskANNSideEntry> {
        let start = index.checked_mul(ENTRY_BYTES)?;
        let end = start.checked_add(ENTRY_BYTES)?;
        Some(
            read_entry(self.dimensions, self.entries.get(start..end)?)
                .expect("validated immutable side entry"),
        )
    }
}

impl DiskANNSideLayout {
    pub fn new(
        generation: DiskANNGeneration,
        dimensions: u32,
        count: u64,
    ) -> StorageBackendResult<Self> {
        if dimensions == 0 {
            return Err(invalid("side stream dimensions must be positive"));
        }
        Ok(Self {
            generation,
            dimensions,
            count,
        })
    }

    pub fn encode(
        self,
        start: u64,
        entries: &[DiskANNSideEntry],
        control: &StorageReadControl,
    ) -> StorageBackendResult<BudgetedVec<u8>> {
        control.check()?;
        self.check_extent(start, entries.len())?;
        let size = entries
            .len()
            .checked_mul(ENTRY_BYTES)
            .and_then(|len| len.checked_add(METADATA_BYTES))
            .ok_or_else(|| invalid("side batch size overflow"))?;
        let mut bytes = record::begin(MAGIC, self.generation, size, control)?;
        for value in [self.dimensions, CLASSIFICATION_REVISION] {
            bytes.extend_from_slice(&value.to_le_bytes())?;
        }
        for value in [self.count, start, entries.len() as u64] {
            bytes.extend_from_slice(&value.to_le_bytes())?;
        }
        let mut previous = None;
        for &entry in entries {
            control.check()?;
            if entry.dimensions != self.dimensions {
                return Err(invalid("side entry dimensions differ"));
            }
            ordered(&mut previous, entry)?;
            bytes.extend_from_slice(&entry.doc.to_le_bytes())?;
            bytes.extend_from_slice(&entry.ordinal.to_le_bytes())?;
            let reason: u32 = match entry.reason {
                ExactVectorReason::ZeroNorm => 1,
                ExactVectorReason::NonFiniteNorm => 2,
            };
            bytes.extend_from_slice(&reason.to_le_bytes())?;
            bytes.extend_from_slice(&entry.version.writer.database().as_bytes())?;
            bytes.extend_from_slice(&entry.version.writer.allocation().to_le_bytes())?;
            bytes.extend_from_slice(&entry.version.revision.to_le_bytes())?;
        }
        record::finish(bytes, control)
    }

    pub fn decode<'a>(
        self,
        start: u64,
        bytes: &'a [u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNSideBatch<'a>> {
        let body = record::open(MAGIC, self.generation, bytes, control)?;
        if body.len() < METADATA_BYTES || !(body.len() - METADATA_BYTES).is_multiple_of(ENTRY_BYTES)
        {
            return Err(invalid("side batch length differs"));
        }
        if record::u32_at(body, 0)? != self.dimensions
            || record::u32_at(body, 4)? != CLASSIFICATION_REVISION
            || record::u64_at(body, 8)? != self.count
            || record::u64_at(body, 16)? != start
        {
            return Err(invalid(
                "side batch dimensions, revision, corpus or address differs",
            ));
        }
        let entries = &body[METADATA_BYTES..];
        let count = entries.len() / ENTRY_BYTES;
        self.check_extent(start, count)?;
        if record::u64_at(body, 24)? != count as u64 {
            return Err(invalid("side batch count differs"));
        }
        let mut previous = None;
        for bytes in entries.chunks_exact(ENTRY_BYTES) {
            control.check()?;
            ordered(&mut previous, read_entry(self.dimensions, bytes)?)?;
        }
        control.check()?;
        Ok(DiskANNSideBatch {
            dimensions: self.dimensions,
            start,
            entries,
        })
    }

    fn check_extent(self, start: u64, count: usize) -> StorageBackendResult<()> {
        if count == 0
            || start
                .checked_add(count as u64)
                .is_none_or(|end| end > self.count)
        {
            return Err(invalid("side batch exceeds the generation"));
        }
        Ok(())
    }
}

fn ordered(
    previous: &mut Option<(DocId, u32)>,
    entry: DiskANNSideEntry,
) -> StorageBackendResult<()> {
    let key = (entry.doc, entry.ordinal);
    if previous.is_some_and(|last| last >= key) {
        return Err(invalid("side entries must be unique and ordered"));
    }
    *previous = Some(key);
    Ok(())
}

fn read_entry(dimensions: u32, bytes: &[u8]) -> StorageBackendResult<DiskANNSideEntry> {
    let writer = StorageTransactionId::new(
        DatabaseId::from_bytes(field(bytes, 16)?),
        record::u64_at(bytes, 32)?,
    )
    .map_err(|_| invalid("side entry writer allocation is invalid"))?;
    Ok(DiskANNSideEntry {
        dimensions,
        doc: record::u64_at(bytes, 0)?,
        ordinal: record::u32_at(bytes, 8)?,
        reason: match record::u32_at(bytes, 12)? {
            1 => ExactVectorReason::ZeroNorm,
            2 => ExactVectorReason::NonFiniteNorm,
            _ => return Err(invalid("unrecognized numeric side classification")),
        },
        version: DiskANNVectorVersion::new(writer, record::u64_at(bytes, 40)?)?,
    })
}
