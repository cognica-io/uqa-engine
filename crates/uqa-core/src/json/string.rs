//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! String token decoding reserves escaped scratch and the returned UTF-8 buffer together.

use super::{Budgeted, CancellationToken, JsonReadError, MemoryBudget, MemoryError};

/// Decode one quoted JSON string under a shared allowance. The envelope covers `serde_json`'s geometric escape scratch, replacement overlap, and final owned string; it is scoped to this token, not the containing document.
pub fn decode_json_string(
    encoded: impl AsRef<[u8]>,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Budgeted<String>, JsonReadError> {
    cancellation.check()?;
    let encoded = encoded.as_ref();
    let bytes = encoded
        .len()
        .checked_mul(4)
        .and_then(|bytes| bytes.checked_add(16))
        .ok_or(MemoryError::SizeOverflow)?;
    let mut reservation = memory.reserve(bytes)?;
    let value =
        serde_json::from_slice::<String>(encoded).map_err(|_| JsonReadError::InvalidJson)?;
    cancellation.check()?;
    reservation.grow(value.capacity().saturating_sub(reservation.bytes()))?;
    let retained = reservation.split(value.capacity());
    Ok(Budgeted::new(value, retained))
}
