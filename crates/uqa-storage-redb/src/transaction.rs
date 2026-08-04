//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Session transaction state and SQL-savepoint emulation.

use redb::{ReadTransaction, ReadableTable, WriteTransaction};
use uqa_storage::{StorageBackendError, StorageBackendResult};

use crate::error::redb_error;
use crate::store::{DATA_TABLE, GENERATION_KEY, METADATA_TABLE};

#[derive(Default)]
pub(crate) struct SessionState {
    pub(crate) active: Option<ActiveTransaction>,
}

pub(crate) enum ActiveTransaction {
    Read(ReadTransaction),
    Write(Box<WriteState>),
}

pub(crate) struct WriteState {
    pub(crate) transaction: WriteTransaction,
    pub(crate) wrote: bool,
    journal: Vec<UndoEntry>,
    savepoints: Vec<Savepoint>,
}

struct UndoEntry {
    key: Vec<u8>,
    previous: Option<Vec<u8>>,
}

struct Savepoint {
    name: String,
    journal_len: usize,
}

impl WriteState {
    pub(crate) fn new(transaction: WriteTransaction) -> Self {
        Self {
            transaction,
            wrote: false,
            journal: Vec::new(),
            savepoints: Vec::new(),
        }
    }

    pub(crate) fn put(
        &mut self,
        key: &[u8],
        value: &[u8],
        force_undo: bool,
    ) -> StorageBackendResult<()> {
        let previous = {
            let mut table = self
                .transaction
                .open_table(DATA_TABLE)
                .map_err(redb_error)?;
            let previous = table
                .insert(key, value)
                .map_err(redb_error)?
                .map(|value| value.value().to_vec());
            previous
        };
        self.record_undo(key, previous, force_undo);
        self.wrote = true;
        Ok(())
    }

    pub(crate) fn delete(&mut self, key: &[u8], force_undo: bool) -> StorageBackendResult<bool> {
        let previous = {
            let mut table = self
                .transaction
                .open_table(DATA_TABLE)
                .map_err(redb_error)?;
            let previous = table
                .remove(key)
                .map_err(redb_error)?
                .map(|value| value.value().to_vec());
            previous
        };
        let removed = previous.is_some();
        if removed {
            self.record_undo(key, previous, force_undo);
            self.wrote = true;
        }
        Ok(removed)
    }

    pub(crate) fn delete_prefix(
        &mut self,
        prefix: &[u8],
        force_undo: bool,
    ) -> StorageBackendResult<usize> {
        let entries = {
            let table = self
                .transaction
                .open_table(DATA_TABLE)
                .map_err(redb_error)?;
            collect_prefix(&table, prefix, None, None)?
        };
        if entries.is_empty() {
            return Ok(0);
        }
        let mut table = self
            .transaction
            .open_table(DATA_TABLE)
            .map_err(redb_error)?;
        for (key, value) in &entries {
            table.remove(key.as_slice()).map_err(redb_error)?;
            if force_undo || !self.savepoints.is_empty() {
                self.journal.push(UndoEntry {
                    key: key.clone(),
                    previous: Some(value.clone()),
                });
            }
        }
        self.wrote = true;
        Ok(entries.len())
    }

    pub(crate) fn savepoint(&mut self, name: &str) {
        self.savepoints.push(Savepoint {
            name: name.to_string(),
            journal_len: self.journal.len(),
        });
    }

    pub(crate) fn release_savepoint(&mut self, name: &str) -> StorageBackendResult<()> {
        let position = self.savepoint_position(name)?;
        self.savepoints.truncate(position);
        if self.savepoints.is_empty() {
            self.journal.clear();
        }
        Ok(())
    }

    pub(crate) fn rollback_to_savepoint(&mut self, name: &str) -> StorageBackendResult<()> {
        let position = self.savepoint_position(name)?;
        let journal_len = self.savepoints[position].journal_len;
        self.rollback_to_offset(journal_len)?;
        self.savepoints.truncate(position + 1);
        Ok(())
    }

    pub(crate) fn journal_len(&self) -> usize {
        self.journal.len()
    }

    pub(crate) fn rollback_to_offset(&mut self, offset: usize) -> StorageBackendResult<()> {
        let mut table = self
            .transaction
            .open_table(DATA_TABLE)
            .map_err(redb_error)?;
        for undo in self.journal[offset..].iter().rev() {
            match undo.previous.as_deref() {
                Some(value) => {
                    table
                        .insert(undo.key.as_slice(), value)
                        .map_err(redb_error)?;
                }
                None => {
                    table.remove(undo.key.as_slice()).map_err(redb_error)?;
                }
            }
        }
        drop(table);
        self.journal.truncate(offset);
        Ok(())
    }

    pub(crate) fn finish_atomic_scope(&mut self, offset: usize) {
        if self.savepoints.is_empty() {
            self.journal.truncate(offset);
        }
    }

    pub(crate) fn commit(self) -> StorageBackendResult<()> {
        if self.wrote {
            bump_generation(&self.transaction)?;
        }
        self.transaction.commit().map_err(redb_error)
    }

    pub(crate) fn abort(self) -> StorageBackendResult<()> {
        self.transaction.abort().map_err(redb_error)
    }

    fn record_undo(&mut self, key: &[u8], previous: Option<Vec<u8>>, force: bool) {
        if force || !self.savepoints.is_empty() {
            self.journal.push(UndoEntry {
                key: key.to_vec(),
                previous,
            });
        }
    }

    fn savepoint_position(&self, name: &str) -> StorageBackendResult<usize> {
        self.savepoints
            .iter()
            .rposition(|savepoint| savepoint.name == name)
            .ok_or_else(|| StorageBackendError::Other(format!("unknown redb savepoint `{name}`")))
    }
}

pub(crate) fn read_generation<T>(table: &T) -> StorageBackendResult<u64>
where
    T: ReadableTable<&'static str, u64>,
{
    Ok(table
        .get(GENERATION_KEY)
        .map_err(redb_error)?
        .map_or(0, |value| value.value()))
}

pub(crate) fn bump_generation(transaction: &WriteTransaction) -> StorageBackendResult<u64> {
    let mut metadata = transaction.open_table(METADATA_TABLE).map_err(redb_error)?;
    let next = read_generation(&metadata)?.wrapping_add(1);
    metadata.insert(GENERATION_KEY, next).map_err(redb_error)?;
    Ok(next)
}

pub(crate) fn collect_prefix<T>(
    table: &T,
    prefix: &[u8],
    after: Option<&[u8]>,
    limit: Option<usize>,
) -> StorageBackendResult<Vec<(Vec<u8>, Vec<u8>)>>
where
    T: ReadableTable<&'static [u8], &'static [u8]>,
{
    if limit == Some(0) {
        return Ok(Vec::new());
    }
    let start = after.filter(|after| *after >= prefix).unwrap_or(prefix);
    let mut rows = Vec::new();
    for entry in table.range(start..).map_err(redb_error)? {
        let (key, value) = entry.map_err(redb_error)?;
        let key = key.value();
        if !key.starts_with(prefix) {
            break;
        }
        if after.is_some_and(|after| key <= after) {
            continue;
        }
        rows.push((key.to_vec(), value.value().to_vec()));
        if limit.is_some_and(|limit| rows.len() == limit) {
            break;
        }
    }
    Ok(rows)
}

pub(crate) fn collect_prefix_keys<T>(
    table: &T,
    prefix: &[u8],
    after: Option<&[u8]>,
    limit: usize,
) -> StorageBackendResult<Vec<Vec<u8>>>
where
    T: ReadableTable<&'static [u8], &'static [u8]>,
{
    if limit == 0 {
        return Ok(Vec::new());
    }
    let start = after.filter(|after| *after >= prefix).unwrap_or(prefix);
    let mut keys = Vec::new();
    for entry in table.range(start..).map_err(redb_error)? {
        let (key, _) = entry.map_err(redb_error)?;
        let key = key.value();
        if !key.starts_with(prefix) {
            break;
        }
        if after.is_some_and(|after| key <= after) {
            continue;
        }
        keys.push(key.to_vec());
        if keys.len() == limit {
            break;
        }
    }
    Ok(keys)
}
