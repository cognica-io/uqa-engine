//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Diagnostics for a retained scalar reinterpreted as a zero-length external varlena. Model the released reference's byte observations inside the owned tuple; never perform its invalid allocation, copy or free.

use crate::SQLError;
use uqa_core::DatumValue;

pub(super) fn external(datum: &DatumValue) -> SQLError {
    let offset = datum.offset() as usize;
    // REL_18_6 detoast_attr falls through an unknown external tag to its short-header branch. Its three-byte destination receives a wrapping-size copy from source + 1 to destination + 4. The aarch64 reference's final copy ends at destination + 3: destination[-8..0] observes source[-11..-3], and destination[0..3] observes source[-3..0]. The fourth header byte remains zero. Only these bounded observations are reproduced here.
    let before = offset
        .checked_sub(11)
        .and_then(|start| datum.bytes().get(start..offset));
    let (header, length) = before.map_or((0, 0), |before| {
        let header = u64::from_le_bytes(before[..8].try_into().expect("eight header bytes"));
        let length = u32::from_le_bytes([before[8], before[9], before[10], 0]);
        (header, length)
    });
    if length == 0 {
        return internal(format!(
            "invalid memory alloc request size {}",
            u64::MAX - 2
        ));
    }
    let address = datum.bytes().as_ptr().wrapping_add(offset);
    internal(format!(
        "pfree called with invalid pointer {address:p} (header 0x{header:016x})"
    ))
}

fn internal(message: String) -> SQLError {
    SQLError::Routine {
        sqlstate: "XX000".into(),
        message,
    }
}
