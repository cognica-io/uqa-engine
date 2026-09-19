//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fallible primitives for the existing score and occurrence wire format.

use super::{corrupt, StorageBackendResult};
use uqa_core::memory::BudgetedVec;

pub(super) fn put_varint(output: &mut BudgetedVec<u8>, mut value: u64) -> StorageBackendResult<()> {
    let mut encoded = [0_u8; 10];
    let mut length = 0;
    while value >= 0x80 {
        encoded[length] = (value as u8 & 0x7f) | 0x80;
        length += 1;
        value >>= 7;
    }
    encoded[length] = value as u8;
    output.extend_from_slice(&encoded[..=length])?;
    Ok(())
}

pub(super) fn put_u32(
    output: &mut BudgetedVec<u8>,
    value: usize,
    field: &str,
) -> StorageBackendResult<()> {
    let value = u32::try_from(value)
        .map_err(|_| corrupt(format!("{field} exceeds the u32 on-disk format")))?;
    output.extend_from_slice(&value.to_le_bytes())?;
    Ok(())
}
