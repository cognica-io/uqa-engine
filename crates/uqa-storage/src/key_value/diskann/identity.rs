//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Never-reused physical handles preserve complete catalog incarnations across names and reopen.

use std::num::NonZeroU64;

use crate::diskann_index::catalog::DiskANNIndexScope;
use crate::key_value::KeyValueRead;
use crate::mvcc::IdentifierRequest;
use crate::{read_control::StorageReadControl, StorageBackendResult};

use super::keys::{database_key, ROOT};
use super::{
    invalid, read_data_identity, state::fixed, stored_data_identity, KeyValueDiskANNStore,
};

const PREFIX: usize = ROOT.len() + 1 + 16;

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
        prefix[ROOT.len()] = 4;
        Self {
            table,
            index,
            allocator: prefix,
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

impl KeyValueDiskANNStore {
    pub(super) fn catalog_handles(
        &self,
        scope: &DiskANNIndexScope,
        control: &StorageReadControl,
    ) -> StorageBackendResult<([u8; 16], u64, u64)> {
        let _writer = self.owner.writer.lock();
        self.idle(control)?;
        scope.check(self.owner.database, control)?;
        let database = stored_data_identity(&*self.owner.store, control)?;
        let keys = Keys::new(database, scope);
        let mut selected = [None; 2];
        self.owner.store.with_read_view(&mut |read| {
            selected = keys.read(read, control)?;
            Ok(())
        })?;
        if let [Some(table), Some(index)] = selected {
            scope.check(self.owner.database, control)?;
            return Ok((database, table, index));
        }
        let count = NonZeroU64::new(selected.iter().filter(|id| id.is_none()).count() as u64)
            .expect("at least one mapping is absent");
        let allocation = self
            .owner
            .store
            .identifier_allocator()
            .ok_or_else(|| invalid("catalog handles require durable identifiers"))?
            .allocate_identifiers(
                &keys.allocator,
                IdentifierRequest::Reserve {
                    minimum: 1,
                    maximum: u64::MAX,
                    count,
                },
            )?;
        let mut next = allocation.watermark() - (count.get() - 1);
        for id in &mut selected {
            if id.is_none() {
                *id = Some(next);
                // The final allocated identity may be u64::MAX; no unused successor is needed.
                next = next.saturating_add(1);
            }
        }
        self.owner.store.with_mutation(&mut |read, batch| {
            scope.check(self.owner.database, control)?;
            if read_data_identity(read, control)? != Some(database) {
                return Err(invalid("catalog handles belong to another data identity"));
            }
            let current = keys.read(read, control)?;
            for (position, key) in [&keys.table[..], &keys.index[..]].into_iter().enumerate() {
                if let Some(id) = current[position] {
                    selected[position] = Some(id);
                } else {
                    let id = selected[position].expect("reserved handle");
                    let mut bytes = [1; 9];
                    bytes[1..].copy_from_slice(&id.to_be_bytes());
                    batch.require_unchanged(key)?;
                    batch.put(key, &bytes)?;
                }
            }
            batch.require_unchanged(&database_key())?;
            scope.check(self.owner.database, control)
        })?;
        let [Some(table), Some(index)] = selected else {
            return Err(invalid("catalog handle selection did not complete"));
        };
        scope.check(self.owner.database, control)?;
        Ok((database, table, index))
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
