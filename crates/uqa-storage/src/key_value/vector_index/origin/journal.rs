//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Per-mutation records use distinct keys, so later writers cannot be overwritten by covered-version reclamation.

use crate::diskann_index::format::DiskANNChangeIdentity;
use crate::key_value::codec;
use crate::StorageBackendResult;

pub(in crate::key_value) const ROOT: &[u8] = b"\0uqa-diskann-changes-v1\0";

pub(in crate::key_value) fn table_prefix(table: &str) -> StorageBackendResult<Vec<u8>> {
    Ok(namespaced(&codec::vector_key_prefix(table)?))
}

pub(in crate::key_value) fn prefix(table: &str, field: &str) -> StorageBackendResult<Vec<u8>> {
    Ok(namespaced(&codec::vector_field_prefix(table, field)?))
}

pub(in crate::key_value) fn key(
    table: &str,
    field: &str,
    identity: DiskANNChangeIdentity,
) -> StorageBackendResult<Vec<u8>> {
    let mut key = prefix(table, field)?;
    key.extend_from_slice(&identity.encode());
    Ok(key)
}

fn namespaced(canonical: &[u8]) -> Vec<u8> {
    let mut prefix = Vec::with_capacity(ROOT.len() + canonical.len());
    prefix.extend_from_slice(ROOT);
    prefix.extend_from_slice(canonical);
    prefix
}
