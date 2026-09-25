//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::memory::BudgetedVec;

use super::{field, invalid, record, DiskANNQuantizationIdentity};
use crate::diskann_index::{metric::checkpoint, pq::PQCodebook};
use crate::{read_control::StorageReadControl, StorageBackendResult};

const MAGIC: [u8; 8] = *b"UQADNCD\0";
const METADATA_BYTES: usize = 72;

#[derive(Debug)]
pub struct DiskANNCodeBatch<'a> {
    start: u64,
    count: u64,
    chunks: usize,
    bytes: &'a [u8],
}

impl DiskANNCodeBatch<'_> {
    pub fn first_node(&self) -> u64 {
        self.start
    }
    pub fn node_count(&self) -> u64 {
        self.count
    }
    pub fn bytes(&self) -> &[u8] {
        self.bytes
    }
    pub fn code(&self, node: u64) -> Option<&[u8]> {
        let offset = node.checked_sub(self.start)?;
        if offset >= self.count {
            return None;
        }
        let start = offset as usize * self.chunks;
        Some(&self.bytes[start..start + self.chunks])
    }
}

impl DiskANNQuantizationIdentity {
    pub fn encode_codes(
        self,
        start: u64,
        codes: &[u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<BudgetedVec<u8>> {
        let count = self.validate_codes(start, codes, control)?;
        let size = METADATA_BYTES
            .checked_add(codes.len())
            .ok_or_else(|| invalid("code record size overflow"))?;
        let mut bytes = record::begin(MAGIC, self.generation, size, control)?;
        for value in [
            self.dimensions,
            self.chunks as u32,
            u32::from(self.centroids),
            PQCodebook::CODEC_REVISION,
        ] {
            bytes.extend_from_slice(&value.to_le_bytes())?;
        }
        for value in [self.nodes, start, count] {
            bytes.extend_from_slice(&value.to_le_bytes())?;
        }
        bytes.extend_from_slice(&self.digest)?;
        for part in codes.chunks(4096) {
            control.check()?;
            bytes.extend_from_slice(part)?;
        }
        record::finish(bytes, control)
    }

    pub fn decode_codes<'a>(
        self,
        start: u64,
        bytes: &'a [u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNCodeBatch<'a>> {
        let body = record::open(MAGIC, self.generation, bytes, control)?;
        if body.len() < METADATA_BYTES {
            return Err(invalid("truncated code batch"));
        }
        for (offset, expected) in [
            (0, self.dimensions),
            (4, self.chunks as u32),
            (8, u32::from(self.centroids)),
            (12, PQCodebook::CODEC_REVISION),
        ] {
            if record::u32_at(body, offset)? != expected {
                return Err(invalid("code batch layout differs from codebook"));
            }
        }
        if record::u64_at(body, 16)? != self.nodes
            || record::u64_at(body, 24)? != start
            || field::<32>(body, 40)? != self.digest
        {
            return Err(invalid("code batch corpus, address or codebook differs"));
        }
        let codes = &body[METADATA_BYTES..];
        let count = self.validate_codes(start, codes, control)?;
        if record::u64_at(body, 32)? != count {
            return Err(invalid("code batch count differs"));
        }
        Ok(DiskANNCodeBatch {
            start,
            count,
            chunks: self.chunks,
            bytes: codes,
        })
    }

    fn validate_codes(
        self,
        start: u64,
        codes: &[u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<u64> {
        control.check()?;
        if codes.is_empty() || !codes.len().is_multiple_of(self.chunks) {
            return Err(invalid("codes must contain complete nonempty node entries"));
        }
        let count = (codes.len() / self.chunks) as u64;
        if start.checked_add(count).is_none_or(|end| end > self.nodes) {
            return Err(invalid("code batch exceeds the generation"));
        }
        for (offset, &label) in codes.iter().enumerate() {
            checkpoint(offset, control)?;
            if u16::from(label) >= self.centroids {
                return Err(invalid("code label exceeds actual centroid count"));
            }
        }
        control.check()?;
        Ok(count)
    }
}
