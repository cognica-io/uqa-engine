//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Never-reused physical handles preserve complete catalog incarnations across names and reopen.

use crate::diskann_index::catalog::DiskANNIndexScope;
use crate::key_value::KeyValueRead;
use crate::{read_control::StorageReadControl, StorageBackendResult};

use super::keys::{database_key, ROOT};
use super::{invalid, read_data_identity, state::fixed};

mod allocation;
mod reclamation;
pub use reclamation::KeyValueDiskANNMappingMaintenance;

const PREFIX: usize = ROOT.len() + 1 + 16;
const TABLE_GUARD_TAG: u8 = 6;
const INDEX_GUARD_TAG: u8 = 7;

pub(super) fn require_mapping(
    scope: &DiskANNIndexScope,
    generation: crate::diskann_index::format::DiskANNGeneration,
    read: &dyn KeyValueRead,
    batch: &mut dyn crate::KeyValueBatch,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    validate_mapping(scope, generation, read, control)?;
    let key = database_key();
    let keys = Keys::new(generation.database(), scope);
    for key in [&key[..], &keys.table[..], &keys.index[..]] {
        super::publication::require_observed(read, key, batch)?;
    }
    Ok(())
}

pub(super) fn validate_mapping(
    scope: &DiskANNIndexScope,
    generation: crate::diskann_index::format::DiskANNGeneration,
    read: &dyn KeyValueRead,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let key = database_key();
    let history = read
        .record_revision(&key)?
        .and_then(|revision| revision.record_database())
        .ok_or_else(|| invalid("physical data marker has no versioned identity"))?;
    scope.check(history, control)?;
    if read_data_identity(read, control)? != Some(generation.database()) {
        return Err(invalid("publication belongs to another data identity"));
    }
    let keys = Keys::new(generation.database(), scope);
    if keys.read(read, control)? != [Some(generation.table()), Some(generation.index())] {
        return Err(invalid(
            "publication does not match catalog physical handles",
        ));
    }
    Ok(())
}

struct Keys {
    table: [u8; PREFIX + 32],
    index: [u8; PREFIX + 48],
    allocator: [u8; PREFIX],
    table_guard: [u8; PREFIX + 32],
    index_guard: [u8; PREFIX + 48],
}

impl Keys {
    fn new(database: [u8; 16], scope: &DiskANNIndexScope) -> Self {
        let mut prefix = [0; PREFIX];
        prefix[..ROOT.len()].copy_from_slice(ROOT);
        prefix[ROOT.len() + 1..].copy_from_slice(&database);
        let mut table = [0; PREFIX + 32];
        prefix[ROOT.len()] = 2;
        table[..PREFIX].copy_from_slice(&prefix);
        table[PREFIX..PREFIX + 16].copy_from_slice(&scope.table);
        table[PREFIX + 16..].copy_from_slice(&scope.storage);
        let mut index = [0; PREFIX + 48];
        index[..table.len()].copy_from_slice(&table);
        index[ROOT.len()] = 3;
        index[table.len()..].copy_from_slice(&scope.index);
        let mut table_guard = table;
        table_guard[ROOT.len()] = TABLE_GUARD_TAG;
        let mut index_guard = index;
        index_guard[ROOT.len()] = INDEX_GUARD_TAG;
        prefix[ROOT.len()] = 4;
        Self {
            table,
            index,
            allocator: prefix,
            table_guard,
            index_guard,
        }
    }

    fn read(
        &self,
        read: &dyn KeyValueRead,
        control: &StorageReadControl,
    ) -> StorageBackendResult<[Option<u64>; 2]> {
        Ok([
            load(read, &self.table, control)?,
            load(read, &self.index, control)?,
        ])
    }
}

fn load(
    read: &dyn KeyValueRead,
    key: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Option<u64>> {
    fixed::<9>(control, |visit| {
        read.visit_value_bounded(key, 9, control, visit)
    })?
    .map(|bytes| {
        let id = u64::from_be_bytes(bytes[1..].try_into().expect("fixed handle"));
        if bytes[0] != 1 || id == 0 {
            return Err(invalid("invalid catalog physical handle"));
        }
        Ok(id)
    })
    .transpose()
}
