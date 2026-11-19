//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable certification of integer-key document identities.

use crate::{
    catalog::input::{metadata, CatalogInputLimits},
    read_control::StorageReadControl,
    CatalogFacade, StorageBackendResult,
};
use uqa_core::image_io::AsyncImageInput;

/// Present after every eligible table has been checked against its physical document identities.
pub const VERIFIED_KEY: &str = "uqa.integer_key_identities.verified.v1";
const INDEXED_PREFIX: &str = "uqa.integer_key_identities.indexed.v1:";

/// Nonempty metadata at this key records a table whose integer primary key must use its index.
pub fn indexed_key(object_id: [u8; 16]) -> String {
    let bytes = suffix_bytes(&object_id);
    let suffix = std::str::from_utf8(&bytes).expect("hexadecimal identity is ASCII");
    format!("{INDEXED_PREFIX}{suffix}")
}

fn suffix_bytes(object_id: &[u8; 16]) -> [u8; 32] {
    let mut output = [0; 32];
    for (byte, digits) in object_id.iter().zip(output.chunks_exact_mut(2)) {
        digits[0] = b"0123456789abcdef"[usize::from(byte >> 4)];
        digits[1] = b"0123456789abcdef"[usize::from(byte & 15)];
    }
    output
}

/// Select the existing mapping mode for an eligible table. Verification is a presence marker, including empty text; only a nonempty table-specific marker disables direct identity mapping.
pub fn load(catalog: &dyn CatalogFacade, object_id: [u8; 16]) -> StorageBackendResult<bool> {
    if catalog.get_metadata(VERIFIED_KEY)?.is_none() {
        return Ok(false);
    }
    Ok(catalog
        .get_metadata(&indexed_key(object_id))?
        .is_none_or(|value| value.is_empty()))
}

/// Select the same two exact records through bounded metadata input. An absent database marker completes without inspecting the table marker; keys, values and both cancellation owners retain their original controls.
pub async fn load_from(
    catalog: &dyn CatalogFacade,
    object_id: [u8; 16],
    limits: CatalogInputLimits,
    control: &StorageReadControl,
    input: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<bool> {
    if metadata::read_from(catalog, VERIFIED_KEY, "", limits, control, input)
        .await?
        .is_none()
    {
        return Ok(false);
    }
    let bytes = suffix_bytes(&object_id);
    let suffix = std::str::from_utf8(&bytes).expect("hexadecimal identity is ASCII");
    Ok(
        metadata::read_from(catalog, INDEXED_PREFIX, suffix, limits, control, input)
            .await?
            .is_none_or(|value| value.is_empty()),
    )
}
