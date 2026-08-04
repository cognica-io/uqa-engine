//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;

use parking_lot::Mutex;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use uqa_storage::{KeyValueBatch, KeyValueStore, StorageBackendError, StorageBackendResult};

use crate::batch::{BatchOperation, RedbBatch};
use crate::error::redb_error;
use crate::transaction::{
    collect_prefix, collect_prefix_keys, read_generation, ActiveTransaction, SessionState,
    WriteState,
};

pub(crate) const DATA_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("uqa_key_value");
pub(crate) const METADATA_TABLE: TableDefinition<&str, u64> =
    TableDefinition::new("uqa_storage_metadata");
pub(crate) const GENERATION_KEY: &str = "change_version";

/// One transaction-isolated session over a shared redb database.
pub struct RedbKeyValueStore {
    database: Arc<Database>,
    state: Mutex<SessionState>,
}

impl RedbKeyValueStore {
    pub(crate) fn new(database: Arc<Database>) -> Self {
        Self {
            database,
            state: Mutex::new(SessionState::default()),
        }
    }

    pub(crate) fn commit_batch(&self, operations: &[BatchOperation]) -> StorageBackendResult<()> {
        let mut state = self.state.lock();
        match state.active.as_mut() {
            Some(ActiveTransaction::Read(_)) => Err(read_only_error()),
            Some(ActiveTransaction::Write(write)) => {
                let offset = write.journal_len();
                if let Err(error) = apply_operations(write, operations, true) {
                    return match write.rollback_to_offset(offset) {
                        Ok(()) => Err(error),
                        Err(rollback) => Err(StorageBackendError::Other(format!(
                            "{error}; redb batch rollback also failed: {rollback}"
                        ))),
                    };
                }
                write.finish_atomic_scope(offset);
                Ok(())
            }
            None => {
                drop(state);
                let mut write = self.new_write_state()?;
                apply_operations(&mut write, operations, false)?;
                write.commit()
            }
        }
    }

    fn new_write_state(&self) -> StorageBackendResult<WriteState> {
        Ok(WriteState::new(
            self.database.begin_write().map_err(redb_error)?,
        ))
    }

    fn autocommit_write(
        &self,
        operation: impl FnOnce(&mut WriteState) -> StorageBackendResult<()>,
    ) -> StorageBackendResult<()> {
        let mut write = self.new_write_state()?;
        operation(&mut write)?;
        write.commit()
    }
}

impl KeyValueStore for RedbKeyValueStore {
    fn get(&self, key: &[u8]) -> StorageBackendResult<Option<Vec<u8>>> {
        let state = self.state.lock();
        match state.active.as_ref() {
            Some(ActiveTransaction::Read(transaction)) => {
                let table = transaction.open_table(DATA_TABLE).map_err(redb_error)?;
                read_value(&table, key)
            }
            Some(ActiveTransaction::Write(write)) => {
                let table = write
                    .transaction
                    .open_table(DATA_TABLE)
                    .map_err(redb_error)?;
                read_value(&table, key)
            }
            None => {
                drop(state);
                let transaction = self.database.begin_read().map_err(redb_error)?;
                let table = transaction.open_table(DATA_TABLE).map_err(redb_error)?;
                read_value(&table, key)
            }
        }
    }

    fn put(&self, key: &[u8], value: &[u8]) -> StorageBackendResult<()> {
        let mut state = self.state.lock();
        match state.active.as_mut() {
            Some(ActiveTransaction::Read(_)) => Err(read_only_error()),
            Some(ActiveTransaction::Write(write)) => write.put(key, value, false),
            None => {
                drop(state);
                self.autocommit_write(|write| write.put(key, value, false))
            }
        }
    }

    fn delete(&self, key: &[u8]) -> StorageBackendResult<()> {
        let mut state = self.state.lock();
        match state.active.as_mut() {
            Some(ActiveTransaction::Read(_)) => Err(read_only_error()),
            Some(ActiveTransaction::Write(write)) => write.delete(key, false).map(|_| ()),
            None => {
                drop(state);
                self.autocommit_write(|write| write.delete(key, false).map(|_| ()))
            }
        }
    }

    fn scan_prefix(&self, prefix: &[u8]) -> StorageBackendResult<Vec<(Vec<u8>, Vec<u8>)>> {
        self.scan(prefix, None, None)
    }

    fn scan_prefix_keys_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
    ) -> StorageBackendResult<Vec<Vec<u8>>> {
        self.scan_keys(prefix, after, limit)
    }

    fn first_prefix_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
    ) -> StorageBackendResult<Option<(Vec<u8>, Vec<u8>)>> {
        Ok(self.scan(prefix, after, Some(1))?.into_iter().next())
    }

    fn delete_prefix(&self, prefix: &[u8]) -> StorageBackendResult<usize> {
        let mut state = self.state.lock();
        match state.active.as_mut() {
            Some(ActiveTransaction::Read(_)) => Err(read_only_error()),
            Some(ActiveTransaction::Write(write)) => {
                let offset = write.journal_len();
                match write.delete_prefix(prefix, true) {
                    Ok(deleted) => {
                        write.finish_atomic_scope(offset);
                        Ok(deleted)
                    }
                    Err(error) => match write.rollback_to_offset(offset) {
                        Ok(()) => Err(error),
                        Err(rollback) => Err(StorageBackendError::Other(format!(
                            "{error}; redb prefix-delete rollback also failed: {rollback}"
                        ))),
                    },
                }
            }
            None => {
                drop(state);
                let mut write = self.new_write_state()?;
                let deleted = write.delete_prefix(prefix, false)?;
                write.commit()?;
                Ok(deleted)
            }
        }
    }

    fn batch(&self) -> Box<dyn KeyValueBatch + '_> {
        Box::new(RedbBatch::new(self))
    }

    fn begin_transaction(&self) -> StorageBackendResult<()> {
        let mut state = self.state.lock();
        ensure_inactive(&state)?;
        state.active = Some(ActiveTransaction::Write(Box::new(self.new_write_state()?)));
        Ok(())
    }

    fn begin_read_transaction(&self) -> StorageBackendResult<()> {
        let mut state = self.state.lock();
        ensure_inactive(&state)?;
        let transaction = self.database.begin_read().map_err(redb_error)?;
        state.active = Some(ActiveTransaction::Read(transaction));
        Ok(())
    }

    fn in_transaction(&self) -> bool {
        self.state.lock().active.is_some()
    }

    fn transaction_has_written(&self) -> StorageBackendResult<bool> {
        Ok(matches!(
            self.state.lock().active.as_ref(),
            Some(ActiveTransaction::Write(write)) if write.wrote
        ))
    }

    fn change_version(&self) -> StorageBackendResult<Option<u64>> {
        let state = self.state.lock();
        let version = match state.active.as_ref() {
            Some(ActiveTransaction::Read(transaction)) => {
                let table = transaction.open_table(METADATA_TABLE).map_err(redb_error)?;
                read_generation(&table)?
            }
            Some(ActiveTransaction::Write(write)) => {
                let table = write
                    .transaction
                    .open_table(METADATA_TABLE)
                    .map_err(redb_error)?;
                read_generation(&table)?
            }
            None => {
                drop(state);
                let transaction = self.database.begin_read().map_err(redb_error)?;
                let table = transaction.open_table(METADATA_TABLE).map_err(redb_error)?;
                return Ok(Some(read_generation(&table)?));
            }
        };
        Ok(Some(version))
    }

    fn commit_transaction(&self) -> StorageBackendResult<()> {
        let active = self
            .state
            .lock()
            .active
            .take()
            .ok_or_else(no_transaction_error)?;
        match active {
            ActiveTransaction::Read(transaction) => transaction.close().map_err(redb_error),
            ActiveTransaction::Write(write) => (*write).commit(),
        }
    }

    fn rollback_transaction(&self) -> StorageBackendResult<()> {
        let active = self
            .state
            .lock()
            .active
            .take()
            .ok_or_else(no_transaction_error)?;
        match active {
            ActiveTransaction::Read(transaction) => transaction.close().map_err(redb_error),
            ActiveTransaction::Write(write) => (*write).abort(),
        }
    }

    fn savepoint(&self, name: &str) -> StorageBackendResult<()> {
        let mut state = self.state.lock();
        match state.active.as_mut() {
            Some(ActiveTransaction::Write(write)) => {
                write.savepoint(name);
                Ok(())
            }
            Some(ActiveTransaction::Read(_)) => Err(read_only_error()),
            None => Err(no_transaction_error()),
        }
    }

    fn release_savepoint(&self, name: &str) -> StorageBackendResult<()> {
        let mut state = self.state.lock();
        match state.active.as_mut() {
            Some(ActiveTransaction::Write(write)) => write.release_savepoint(name),
            Some(ActiveTransaction::Read(_)) => Err(read_only_error()),
            None => Err(no_transaction_error()),
        }
    }

    fn rollback_to_savepoint(&self, name: &str) -> StorageBackendResult<()> {
        let mut state = self.state.lock();
        match state.active.as_mut() {
            Some(ActiveTransaction::Write(write)) => write.rollback_to_savepoint(name),
            Some(ActiveTransaction::Read(_)) => Err(read_only_error()),
            None => Err(no_transaction_error()),
        }
    }
}

impl RedbKeyValueStore {
    fn scan(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: Option<usize>,
    ) -> StorageBackendResult<Vec<(Vec<u8>, Vec<u8>)>> {
        let state = self.state.lock();
        match state.active.as_ref() {
            Some(ActiveTransaction::Read(transaction)) => {
                let table = transaction.open_table(DATA_TABLE).map_err(redb_error)?;
                collect_prefix(&table, prefix, after, limit)
            }
            Some(ActiveTransaction::Write(write)) => {
                let table = write
                    .transaction
                    .open_table(DATA_TABLE)
                    .map_err(redb_error)?;
                collect_prefix(&table, prefix, after, limit)
            }
            None => {
                drop(state);
                let transaction = self.database.begin_read().map_err(redb_error)?;
                let table = transaction.open_table(DATA_TABLE).map_err(redb_error)?;
                collect_prefix(&table, prefix, after, limit)
            }
        }
    }

    fn scan_keys(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
    ) -> StorageBackendResult<Vec<Vec<u8>>> {
        let state = self.state.lock();
        match state.active.as_ref() {
            Some(ActiveTransaction::Read(transaction)) => {
                let table = transaction.open_table(DATA_TABLE).map_err(redb_error)?;
                collect_prefix_keys(&table, prefix, after, limit)
            }
            Some(ActiveTransaction::Write(write)) => {
                let table = write
                    .transaction
                    .open_table(DATA_TABLE)
                    .map_err(redb_error)?;
                collect_prefix_keys(&table, prefix, after, limit)
            }
            None => {
                drop(state);
                let transaction = self.database.begin_read().map_err(redb_error)?;
                let table = transaction.open_table(DATA_TABLE).map_err(redb_error)?;
                collect_prefix_keys(&table, prefix, after, limit)
            }
        }
    }
}

pub(crate) fn initialize_database(database: &Database) -> StorageBackendResult<()> {
    let transaction = database.begin_write().map_err(redb_error)?;
    transaction.open_table(DATA_TABLE).map_err(redb_error)?;
    {
        let mut metadata = transaction.open_table(METADATA_TABLE).map_err(redb_error)?;
        if metadata.get(GENERATION_KEY).map_err(redb_error)?.is_none() {
            metadata.insert(GENERATION_KEY, 0).map_err(redb_error)?;
        }
    }
    transaction.commit().map_err(redb_error)
}

fn apply_operations(
    write: &mut WriteState,
    operations: &[BatchOperation],
    force_undo: bool,
) -> StorageBackendResult<()> {
    for operation in operations {
        match operation {
            BatchOperation::Put(key, value) => write.put(key, value, force_undo)?,
            BatchOperation::Delete(key) => {
                write.delete(key, force_undo)?;
            }
            BatchOperation::DeletePrefix(prefix) => {
                write.delete_prefix(prefix, force_undo)?;
            }
        }
    }
    Ok(())
}

fn read_value<T>(table: &T, key: &[u8]) -> StorageBackendResult<Option<Vec<u8>>>
where
    T: ReadableTable<&'static [u8], &'static [u8]>,
{
    Ok(table
        .get(key)
        .map_err(redb_error)?
        .map(|value| value.value().to_vec()))
}

fn ensure_inactive(state: &SessionState) -> StorageBackendResult<()> {
    if state.active.is_some() {
        Err(StorageBackendError::Other(
            "a redb transaction is already open in this session".into(),
        ))
    } else {
        Ok(())
    }
}

fn no_transaction_error() -> StorageBackendError {
    StorageBackendError::Other("no open redb transaction".into())
}

fn read_only_error() -> StorageBackendError {
    StorageBackendError::Other("cannot write in a read-only redb transaction".into())
}
