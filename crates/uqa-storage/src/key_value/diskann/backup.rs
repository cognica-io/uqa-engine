//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Image membership follows the generation lifecycle at the retained record cut.

use crate::{
    mvcc::{CommittedRecordSnapshot, VersionError},
    read_control::StorageReadControl,
    StorageBackendResult,
};

use super::{
    invalid,
    keys::{state_generation, Keys, Kind, PREFIX_BYTES, ROOT},
    state::{State, STATE_BYTES},
    DiskANNStageStatus,
};

/// Publication installs a head and Published state atomically; replacement or DROP retires the previous state in that same transaction. Thus only Published generation payloads belong to the committed image. Seals and physical build leases alone never establish membership. Mapping identities and allocator maxima remain in the image independently of this payload projection.
pub(in crate::key_value) fn include(
    records: &dyn CommittedRecordSnapshot,
    key: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<(bool, usize)> {
    diskann_backup_membership(key, |state_key, bytes| {
        let chunk = records
            .read_value_chunk(state_key, 0, bytes, control)
            .map_err(VersionError::into_storage_error)?
            .ok_or_else(|| invalid("backup generation has no state"))?;
        if chunk.total_bytes() != Some(STATE_BYTES) || chunk.copied() != STATE_BYTES {
            return Err(invalid("backup generation state has invalid length"));
        }
        Ok(STATE_BYTES)
    })
}

/// Select a record using the owning generation codec. The mapping callback reads the exact state at its retained cut into the fixed output and returns the actual source metadata bytes read, including its own envelope. No callback occurs for a nongeneration key. Invalid or missing state must fail rather than select a provisional generation.
pub fn diskann_backup_membership(
    key: &[u8],
    read_state: impl FnOnce(&[u8], &mut [u8]) -> StorageBackendResult<usize>,
) -> StorageBackendResult<(bool, usize)> {
    if !key.starts_with(ROOT) || key.get(ROOT.len()) != Some(&1) {
        return Ok((true, 0));
    }
    if key.len() < PREFIX_BYTES + 1 {
        return Err(invalid("incomplete backup generation key"));
    }
    let mut state_key = [0; PREFIX_BYTES + 1];
    state_key[..PREFIX_BYTES].copy_from_slice(&key[..PREFIX_BYTES]);
    let generation = state_generation(&state_key)?;
    let keys = Keys::new(generation);
    keys.decode(key)?;
    let state_key = keys.key(Kind::State);
    let mut bytes = [0; STATE_BYTES];
    let read_bytes = read_state(state_key.as_ref(), &mut bytes)?;
    Ok((
        State::decode(bytes)?.status == DiskANNStageStatus::Published,
        read_bytes,
    ))
}

#[cfg(test)]
mod tests;
