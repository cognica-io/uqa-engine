//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validated positional slices borrow their encoded owner without an offset table allocation.

use super::{corrupt, read_u32, StorageBackendResult, HEADER_LEN};

pub(super) struct PositionDirectory<'a> {
    offsets: &'a [u8],
    data: &'a [u8],
    count: usize,
}
impl<'a> PositionDirectory<'a> {
    pub fn new(
        blob: &'a [u8],
        expected_count: usize,
        poll: &mut dyn FnMut() -> StorageBackendResult<()>,
    ) -> StorageBackendResult<Self> {
        poll()?;
        let count = read_u32(blob, 8)? as usize;
        let offset_count = read_u32(blob, 12)? as usize;
        if count != expected_count || offset_count != count.saturating_add(1) {
            return Err(corrupt("positions posting count mismatch"));
        }
        let data_start = HEADER_LEN
            .checked_add(
                offset_count
                    .checked_mul(size_of::<u32>())
                    .ok_or_else(|| corrupt("positions offset table size overflow"))?,
            )
            .ok_or_else(|| corrupt("positions data offset overflow"))?;
        if data_start > blob.len() {
            return Err(corrupt("truncated positions offset table"));
        }
        let output = Self {
            offsets: &blob[HEADER_LEN..data_start],
            data: &blob[data_start..],
            count,
        };
        let mut previous = 0;
        for index in 0..offset_count {
            poll()?;
            let offset = read_u32(output.offsets, index * 4)? as usize;
            if (index == 0 && offset != 0) || offset < previous || offset > output.data.len() {
                return Err(corrupt("invalid positions payload offsets"));
            }
            previous = offset;
        }
        if previous != output.data.len() {
            return Err(corrupt("invalid positions payload offsets"));
        }
        poll()?;
        Ok(output)
    }

    pub fn entry(&self, index: usize) -> StorageBackendResult<&'a [u8]> {
        if index >= self.count {
            return Err(corrupt("positions entry index is out of bounds"));
        }
        let start = read_u32(self.offsets, index * 4)? as usize;
        let end = read_u32(self.offsets, (index + 1) * 4)? as usize;
        Ok(&self.data[start..end])
    }
}
