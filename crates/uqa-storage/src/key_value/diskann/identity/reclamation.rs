//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Finite mapping cleanup requires absent generations and fences every new preparation.

use std::sync::Arc;

use uqa_core::memory::MemoryReservation;

use crate::diskann_index::format::DiskANNGeneration;
use crate::key_value::KeyValueRead;
use crate::read_control::StorageReadControl;
use crate::{KeyValueStore, StorageBackendResult};

use super::super::{
    invalid, keys::Keys as GenerationKeys, read_data_identity, validate_session,
    KeyValueDiskANNStore,
};
use super::{
    allocation::rejected_mapping_conflict, database_key, fixed, load, INDEX_GUARD_TAG, PREFIX,
    ROOT, TABLE_GUARD_TAG,
};

const TABLE_BYTES: usize = PREFIX + 32;
const INDEX_BYTES: usize = PREFIX + 48;

#[derive(Clone, Copy)]
struct MappingKey {
    bytes: [u8; INDEX_BYTES],
    len: usize,
}

impl MappingKey {
    fn decode(key: &[u8], database: [u8; 16]) -> StorageBackendResult<Self> {
        let valid = (key.len() == TABLE_BYTES && key[ROOT.len()] == 2)
            || (key.len() == INDEX_BYTES && key[ROOT.len()] == 3);
        if !valid || !key.starts_with(ROOT) || key[ROOT.len() + 1..PREFIX] != database {
            return Err(invalid(
                "mapping discovery requires a complete catalog identity",
            ));
        }
        if key[PREFIX..].chunks_exact(16).any(|id| id == [0; 16]) {
            return Err(invalid("mapping catalog identity must be nonzero"));
        }
        let mut bytes = [0; INDEX_BYTES];
        bytes[..key.len()].copy_from_slice(key);
        Ok(Self {
            bytes,
            len: key.len(),
        })
    }

    fn table(self) -> Self {
        let mut key = self;
        key.bytes[ROOT.len()] = 2;
        key.len = TABLE_BYTES;
        key
    }

    fn guard(self) -> Self {
        let mut key = self;
        key.bytes[ROOT.len()] = if key.len == TABLE_BYTES {
            TABLE_GUARD_TAG
        } else {
            INDEX_GUARD_TAG
        };
        key
    }

    fn head_prefix(self) -> Self {
        let mut bytes = [0; INDEX_BYTES];
        let prefix = super::super::publication::HEAD_PREFIX;
        bytes[..prefix.len()].copy_from_slice(prefix);
        let identity = &self.bytes[PREFIX..self.len];
        bytes[prefix.len()..prefix.len() + identity.len()].copy_from_slice(identity);
        Self {
            bytes,
            len: prefix.len() + identity.len(),
        }
    }

    fn index_prefix(self) -> Self {
        let mut key = self.table();
        key.bytes[ROOT.len()] = 3;
        key
    }
}

impl AsRef<[u8]> for MappingKey {
    fn as_ref(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

/// A finite retained discovery pass over index mappings followed by table mappings. Each step considers one mapping and removes at most that record and its preparation guard. Generation payloads and retained history are separate maintenance owners.
pub struct KeyValueDiskANNMappingMaintenance {
    repository: KeyValueDiskANNStore,
    read: Option<Arc<dyn KeyValueRead + Send + Sync>>,
    database: Option<[u8; 16]>,
    index: bool,
    after: Option<MappingKey>,
    current: Option<MappingKey>,
    control: StorageReadControl,
    _memory: MemoryReservation,
}

impl KeyValueDiskANNMappingMaintenance {
    pub fn start(
        store: &Arc<dyn KeyValueStore>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let memory = control.memory().reserve(std::mem::size_of::<Self>())?;
        let repository = KeyValueDiskANNStore::connect(store, control)?;
        let retained = repository
            .owner
            .store
            .open_retained_read_session(control.cancellation())?;
        validate_session(&*repository.owner.store, &*retained)?;
        let mut read = None;
        retained.with_read_view(&mut |view| {
            read = Some(view.retain(&[ROOT])?);
            Ok(())
        })?;
        let read =
            read.ok_or_else(|| invalid("mapping maintenance did not retain a read boundary"))?;
        let database = read_data_identity(&*read, control)?;
        if database.is_none() && read.contains_prefix_budgeted(ROOT, control)? {
            return Err(invalid("mapping records have no data identity"));
        }
        Ok(Self {
            repository,
            read: database.map(|_| read),
            database,
            index: true,
            after: None,
            current: None,
            control: control.clone(),
            _memory: memory,
        })
    }

    /// `Some(true)` reclaimed a mapping (or observed a peer's deletion); `Some(false)` retained one. `None` finishes the pass. An uncertain outcome preserves this same key until original completion is resolved.
    pub fn step(&mut self) -> StorageBackendResult<Option<bool>> {
        self.control.check()?;
        if self.current.is_none() {
            self.current = self.next_key()?;
        }
        let Some(key) = self.current else {
            self.read = None;
            return Ok(None);
        };
        let reclaimed = self.repository.reclaim_mapping(
            key,
            self.database.expect("discovered data identity"),
            &self.control,
        )?;
        self.after = Some(key);
        self.current = None;
        Ok(Some(reclaimed))
    }

    pub fn commit_pending(&self) -> StorageBackendResult<()> {
        self.repository.commit_pending()
    }
    pub fn rollback_pending(&self) -> StorageBackendResult<()> {
        self.repository.rollback_pending()
    }

    pub fn run(
        store: &Arc<dyn KeyValueStore>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        let mut pass = Self::start(store, control)?;
        while pass.step()?.is_some() {}
        Ok(())
    }

    fn next_key(&mut self) -> StorageBackendResult<Option<MappingKey>> {
        let Some(read) = &self.read else {
            return Ok(None);
        };
        let database = self.database.expect("retained mapping data identity");
        loop {
            self.control.check()?;
            read.control().check()?;
            let mut prefix = [0; PREFIX];
            prefix[..ROOT.len()].copy_from_slice(ROOT);
            prefix[ROOT.len()] = if self.index { 3 } else { 2 };
            prefix[ROOT.len() + 1..].copy_from_slice(&database);
            let mut found = None;
            let mut failed = false;
            let result = read.visit_keys_after(
                &prefix,
                self.after.as_ref().map(AsRef::as_ref),
                1,
                &self.control,
                &mut |bytes| {
                    if failed || found.is_some() {
                        failed = true;
                        return Err(invalid("mapping discovery exceeded its key limit"));
                    }
                    let decoded = MappingKey::decode(bytes, database).and_then(|key| {
                        if !bytes.starts_with(&prefix)
                            || self
                                .after
                                .as_ref()
                                .is_some_and(|after| bytes <= after.as_ref())
                        {
                            return Err(invalid("mapping discovery returned an unordered key"));
                        }
                        Ok(key)
                    });
                    match decoded {
                        Ok(key) => {
                            found = Some(key);
                            Ok(())
                        }
                        Err(error) => {
                            failed = true;
                            Err(error)
                        }
                    }
                },
            );
            if failed {
                return Err(invalid(
                    "mapping discovery suppressed an invalid completion",
                ));
            }
            result?;
            self.control.check()?;
            read.control().check()?;
            if found.is_some() || !self.index {
                return Ok(found);
            }
            self.index = false;
            self.after = None;
        }
    }
}

impl KeyValueDiskANNStore {
    fn reclaim_mapping(
        &self,
        key: MappingKey,
        database: [u8; 16],
        control: &StorageReadControl,
    ) -> StorageBackendResult<bool> {
        let mut reclaimed = false;
        let result = self.mutate(control, &mut |read, batch| {
            if read_data_identity(read, control)? != Some(database) {
                return Err(invalid(
                    "mapping reclamation belongs to another data identity",
                ));
            }
            let Some(handle) = load(read, key.as_ref(), control)? else {
                reclaimed = true;
                return Ok(());
            };
            let table_key = key.table();
            let table = if key.len == TABLE_BYTES {
                handle
            } else {
                load(read, table_key.as_ref(), control)?
                    .ok_or_else(|| invalid("index mapping has no table mapping"))?
            };
            let generation = DiskANNGeneration::new(
                database,
                table,
                if key.len == TABLE_BYTES { 1 } else { handle },
                1,
            )?;
            let physical = GenerationKeys::new(generation);
            let suffix = if key.len == TABLE_BYTES { 16 } else { 8 };
            if read.contains_prefix_budgeted(key.head_prefix().as_ref(), control)?
                || read.contains_prefix_budgeted(
                    &physical.prefix()[..physical.prefix().len() - suffix],
                    control,
                )?
                || (key.len == TABLE_BYTES
                    && read.contains_prefix_budgeted(key.index_prefix().as_ref(), control)?)
            {
                return Ok(());
            }
            let guard_key = key.guard();
            let guard = fixed::<8>(control, |visit| {
                read.visit_value_bounded(guard_key.as_ref(), 8, control, visit)
            })?;
            if guard.is_some_and(|bytes| u64::from_be_bytes(bytes) == 0) {
                return Err(invalid("mapping preparation revision must be nonzero"));
            }
            batch.require_unchanged(&database_key())?;
            batch.require_unchanged(key.as_ref())?;
            batch.require_unchanged(guard_key.as_ref())?;
            batch.delete(key.as_ref())?;
            if guard.is_some() {
                batch.delete(guard_key.as_ref())?;
            }
            reclaimed = true;
            Ok(())
        });
        match result {
            Err(error) if rejected_mapping_conflict(&error) => {
                self.rollback_pending()?;
                Ok(false)
            }
            Err(error) => Err(error),
            Ok(()) => Ok(reclaimed),
        }
    }
}
