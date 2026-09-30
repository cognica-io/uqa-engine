//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The `pg_class` OIDs sequences record when they are created, one catalog metadata entry per object identity. Sequences created before OIDs were recorded derive theirs from their identity.

use std::collections::BTreeMap;
use uqa_storage::{CatalogFacade, StorageBackendError, StorageBackendResult};

/// The catalog metadata key prefix of the entries, which the hexadecimal object identity completes.
pub const SEQUENCE_CATALOG_OID_METADATA_PREFIX: &str = "sequence_catalog_oid:";

/// The OID of the sequence with the object identity: the recorded one, or the one its identity derives.
#[must_use]
pub fn sequence_catalog_oid(oids: &BTreeMap<[u8; 16], u32>, object_id: &[u8; 16]) -> i64 {
    oids.get(object_id).map_or_else(
        || crate::catalog::projection::legacy_sequence_relation_oid(*object_id),
        |oid| i64::from(*oid),
    )
}

/// The recorded OIDs of the catalog's sequences.
pub fn load(catalog: &dyn CatalogFacade) -> StorageBackendResult<BTreeMap<[u8; 16], u32>> {
    catalog
        .metadata_with_prefix(SEQUENCE_CATALOG_OID_METADATA_PREFIX)?
        .into_iter()
        .map(|(key, value)| {
            let encoded = key
                .strip_prefix(SEQUENCE_CATALOG_OID_METADATA_PREFIX)
                .unwrap_or(&key);
            let oid = value.parse::<u32>().map_err(|_| {
                StorageBackendError::Other(format!(
                    "sequence catalog OID entry `{key}` holds invalid OID `{value}`"
                ))
            })?;
            decode_object_id(encoded).map(|object_id| (object_id, oid))
        })
        .collect()
}

/// Record the OID of a sequence being created.
pub fn record(
    catalog: &dyn CatalogFacade,
    object_id: &[u8; 16],
    oid: u32,
) -> StorageBackendResult<()> {
    catalog.set_metadata(&metadata_key(object_id), &oid.to_string())
}

/// Forget the OID of a sequence being removed.
pub fn forget(catalog: &dyn CatalogFacade, object_id: &[u8; 16]) -> StorageBackendResult<()> {
    catalog.delete_metadata(&metadata_key(object_id))
}

fn metadata_key(object_id: &[u8; 16]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut key = String::with_capacity(SEQUENCE_CATALOG_OID_METADATA_PREFIX.len() + 32);
    key.push_str(SEQUENCE_CATALOG_OID_METADATA_PREFIX);
    for byte in object_id {
        key.push(char::from(HEX[usize::from(byte >> 4)]));
        key.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    key
}

fn decode_object_id(encoded: &str) -> StorageBackendResult<[u8; 16]> {
    let invalid = || {
        StorageBackendError::Other(format!(
            "sequence catalog OID entry names invalid object identity `{encoded}`"
        ))
    };
    if encoded.len() != 32 || !encoded.is_ascii() {
        return Err(invalid());
    }
    let mut object_id = [0; 16];
    for (index, byte) in object_id.iter_mut().enumerate() {
        *byte =
            u8::from_str_radix(&encoded[index * 2..index * 2 + 2], 16).map_err(|_| invalid())?;
    }
    Ok(object_id)
}

#[cfg(test)]
mod tests;
