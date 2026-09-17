//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared field identities, vector payloads and non-recycled vector fences.

use super::codec::{read_segment, read_u64};
use crate::{
    mvcc::{VersionError, VersionResult},
    read_control::StorageReadControl,
};
use uqa_core::memory::BudgetedVec;

// Tombstone fences share canonical tensor ownership across IVF and HNSW.
pub(super) const GUARD: u8 = b'y';

pub(super) fn field_end(key: &[u8]) -> VersionResult<usize> {
    let mut offset = 1;
    for _ in 0..2 {
        let name = read_segment(key, &mut offset)?;
        std::str::from_utf8(name)
            .map_err(|_| VersionError::InvalidEncoding("invalid vector name"))?;
    }
    Ok(offset)
}
pub(super) fn tail(key: &[u8], tag: u8, numbers: usize) -> VersionResult<(usize, [u64; 2])> {
    if key.first() != Some(&tag) {
        return Err(VersionError::InvalidEncoding("wrong vector record family"));
    }
    let end = field_end(key)?;
    let mut offset = end;
    let mut output = [0; 2];
    for number in output.iter_mut().take(numbers) {
        *number = read_u64(key, &mut offset)?;
    }
    if offset != key.len() {
        return Err(VersionError::InvalidEncoding("invalid vector key suffix"));
    }
    Ok((end, output))
}
pub(super) fn usize_value(value: u64) -> VersionResult<usize> {
    usize::try_from(value)
        .map_err(|_| VersionError::InvalidEncoding("vector counter exceeds addressable memory"))
}
pub(super) fn ordinal(value: u64) -> VersionResult<u32> {
    u32::try_from(value).map_err(|_| VersionError::InvalidEncoding("invalid vector vector ordinal"))
}
pub(super) fn vector_bytes(
    value: &[u8],
    control: &StorageReadControl,
) -> VersionResult<BudgetedVec<f32>> {
    if !value.len().is_multiple_of(4) {
        return Err(VersionError::InvalidEncoding(
            "invalid vector vector payload",
        ));
    }
    let mut output = BudgetedVec::new(control.memory());
    output.reserve(value.len() / 4)?;
    for bytes in value.chunks_exact(4) {
        control.cancellation().check()?;
        output.push(f32::from_le_bytes(
            bytes.try_into().expect("four-byte chunk"),
        ))?;
    }
    Ok(output)
}
