//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! In-memory implementation of the backend-neutral key/value traits.

use super::{
    BTreeMap, KeyValueBatch, KeyValueBatchOperation, KeyValueStore, Mutex, StorageBackendError,
    StorageBackendResult,
};

/// In-memory Key/Value store used by trait-level tests and future non-SQL
/// fixtures.
#[derive(Debug, Default)]
pub struct MemoryKeyValueStore {
    inner: Mutex<MemoryKeyValueState>,
}

#[derive(Debug, Default, Clone)]
struct MemoryKeyValueState {
    map: BTreeMap<Vec<u8>, Vec<u8>>,
    transactions: Vec<BTreeMap<Vec<u8>, Vec<u8>>>,
    savepoints: Vec<MemorySavepoint>,
    transaction_read_only: bool,
    transaction_written: bool,
    change_version: u64,
}

#[derive(Debug, Clone)]
struct MemorySavepoint {
    name: String,
    snapshot: BTreeMap<Vec<u8>, Vec<u8>>,
}

impl MemoryKeyValueStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl KeyValueStore for MemoryKeyValueStore {
    fn get(&self, key: &[u8]) -> StorageBackendResult<Option<Vec<u8>>> {
        Ok(self.inner.lock().map.get(key).cloned())
    }

    fn contains_key(&self, key: &[u8]) -> StorageBackendResult<bool> {
        Ok(self.inner.lock().map.contains_key(key))
    }

    fn put(&self, key: &[u8], value: &[u8]) -> StorageBackendResult<()> {
        let mut inner = self.inner.lock();
        prepare_write(&mut inner)?;
        inner.map.insert(key.to_vec(), value.to_vec());
        finish_autocommit_write(&mut inner);
        Ok(())
    }

    fn delete(&self, key: &[u8]) -> StorageBackendResult<()> {
        let mut inner = self.inner.lock();
        prepare_write(&mut inner)?;
        inner.map.remove(key);
        finish_autocommit_write(&mut inner);
        Ok(())
    }

    fn scan_prefix(&self, prefix: &[u8]) -> StorageBackendResult<Vec<(Vec<u8>, Vec<u8>)>> {
        Ok(self
            .inner
            .lock()
            .map
            .range(prefix.to_vec()..)
            .take_while(|(key, _)| key.starts_with(prefix))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect())
    }

    fn scan_prefix_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
    ) -> StorageBackendResult<Vec<(Vec<u8>, Vec<u8>)>> {
        use std::ops::Bound::{Excluded, Included, Unbounded};

        if limit == 0 {
            return Ok(Vec::new());
        }
        let inner = self.inner.lock();
        let lower = match after {
            Some(after) if after >= prefix => Excluded(after.to_vec()),
            Some(_) | None => Included(prefix.to_vec()),
        };
        Ok(inner
            .map
            .range((lower, Unbounded))
            .take_while(|(key, _)| key.starts_with(prefix))
            .take(limit)
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect())
    }

    fn scan_prefix_keys_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
    ) -> StorageBackendResult<Vec<Vec<u8>>> {
        use std::ops::Bound::{Excluded, Included, Unbounded};

        if limit == 0 {
            return Ok(Vec::new());
        }
        let inner = self.inner.lock();
        let lower = match after {
            Some(after) if after >= prefix => Excluded(after.to_vec()),
            Some(_) | None => Included(prefix.to_vec()),
        };
        Ok(inner
            .map
            .range((lower, Unbounded))
            .take_while(|(key, _)| key.starts_with(prefix))
            .take(limit)
            .map(|(key, _)| key.clone())
            .collect())
    }

    fn first_prefix_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
    ) -> StorageBackendResult<Option<(Vec<u8>, Vec<u8>)>> {
        use std::ops::Bound::{Excluded, Included, Unbounded};

        let inner = self.inner.lock();
        let lower = match after {
            Some(after) if after >= prefix => Excluded(after.to_vec()),
            Some(_) | None => Included(prefix.to_vec()),
        };
        Ok(inner
            .map
            .range((lower, Unbounded))
            .next()
            .filter(|(key, _)| key.starts_with(prefix))
            .map(|(key, value)| (key.clone(), value.clone())))
    }

    fn delete_prefix(&self, prefix: &[u8]) -> StorageBackendResult<usize> {
        let mut inner = self.inner.lock();
        prepare_write(&mut inner)?;
        let keys = inner
            .map
            .range(prefix.to_vec()..)
            .take_while(|(key, _)| key.starts_with(prefix))
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in &keys {
            inner.map.remove(key);
        }
        finish_autocommit_write(&mut inner);
        Ok(keys.len())
    }

    fn batch(&self) -> Box<dyn KeyValueBatch + '_> {
        Box::new(MemoryKeyValueBatch {
            store: self,
            operations: Vec::new(),
        })
    }

    fn begin_transaction(&self) -> StorageBackendResult<()> {
        let mut inner = self.inner.lock();
        if !inner.transactions.is_empty() {
            return Err(StorageBackendError::Other(
                "a KeyValue transaction is already open".into(),
            ));
        }
        let snapshot = inner.map.clone();
        inner.transactions.push(snapshot);
        inner.transaction_read_only = false;
        inner.transaction_written = false;
        Ok(())
    }

    fn begin_read_transaction(&self) -> StorageBackendResult<()> {
        let mut inner = self.inner.lock();
        if !inner.transactions.is_empty() {
            return Err(StorageBackendError::Other(
                "a KeyValue transaction is already open".into(),
            ));
        }
        let snapshot = inner.map.clone();
        inner.transactions.push(snapshot);
        inner.transaction_read_only = true;
        inner.transaction_written = false;
        Ok(())
    }

    fn in_transaction(&self) -> bool {
        !self.inner.lock().transactions.is_empty()
    }

    fn transaction_has_written(&self) -> StorageBackendResult<bool> {
        Ok(self.inner.lock().transaction_written)
    }

    fn change_version(&self) -> StorageBackendResult<Option<u64>> {
        Ok(Some(self.inner.lock().change_version))
    }

    fn commit_transaction(&self) -> StorageBackendResult<()> {
        let mut inner = self.inner.lock();
        inner.transactions.pop().ok_or_else(|| {
            StorageBackendError::Other("no open KeyValue transaction to commit".into())
        })?;
        if inner.transaction_written {
            inner.change_version = inner.change_version.wrapping_add(1);
        }
        inner.transaction_read_only = false;
        inner.transaction_written = false;
        inner.savepoints.clear();
        Ok(())
    }

    fn rollback_transaction(&self) -> StorageBackendResult<()> {
        let mut inner = self.inner.lock();
        let snapshot = inner.transactions.pop().ok_or_else(|| {
            StorageBackendError::Other("no open KeyValue transaction to roll back".into())
        })?;
        inner.map = snapshot;
        inner.transaction_read_only = false;
        inner.transaction_written = false;
        inner.savepoints.clear();
        Ok(())
    }

    fn savepoint(&self, name: &str) -> StorageBackendResult<()> {
        let mut inner = self.inner.lock();
        if inner.transactions.is_empty() {
            return Err(StorageBackendError::Other(
                "cannot create a savepoint outside a KeyValue transaction".into(),
            ));
        }
        let snapshot = inner.map.clone();
        inner.savepoints.push(MemorySavepoint {
            name: name.to_string(),
            snapshot,
        });
        Ok(())
    }

    fn release_savepoint(&self, name: &str) -> StorageBackendResult<()> {
        let mut inner = self.inner.lock();
        let position = inner
            .savepoints
            .iter()
            .rposition(|savepoint| savepoint.name == name)
            .ok_or_else(|| StorageBackendError::Other(format!("unknown savepoint `{name}`")))?;
        inner.savepoints.truncate(position);
        Ok(())
    }

    fn rollback_to_savepoint(&self, name: &str) -> StorageBackendResult<()> {
        let mut inner = self.inner.lock();
        let position = inner
            .savepoints
            .iter()
            .rposition(|savepoint| savepoint.name == name)
            .ok_or_else(|| StorageBackendError::Other(format!("unknown savepoint `{name}`")))?;
        inner.map = inner.savepoints[position].snapshot.clone();
        inner.savepoints.truncate(position + 1);
        Ok(())
    }
}

struct MemoryKeyValueBatch<'a> {
    store: &'a MemoryKeyValueStore,
    operations: Vec<KeyValueBatchOperation>,
}

impl KeyValueBatch for MemoryKeyValueBatch<'_> {
    fn put(&mut self, key: &[u8], value: &[u8]) -> StorageBackendResult<()> {
        self.operations
            .push(KeyValueBatchOperation::Put(key.to_vec(), value.to_vec()));
        Ok(())
    }

    fn delete(&mut self, key: &[u8]) -> StorageBackendResult<()> {
        self.operations
            .push(KeyValueBatchOperation::Delete(key.to_vec()));
        Ok(())
    }

    fn delete_prefix(&mut self, prefix: &[u8]) -> StorageBackendResult<()> {
        self.operations
            .push(KeyValueBatchOperation::DeletePrefix(prefix.to_vec()));
        Ok(())
    }

    fn commit(self: Box<Self>) -> StorageBackendResult<()> {
        let mut inner = self.store.inner.lock();
        prepare_write(&mut inner)?;
        for operation in self.operations {
            match operation {
                KeyValueBatchOperation::Put(key, value) => {
                    inner.map.insert(key, value);
                }
                KeyValueBatchOperation::Delete(key) => {
                    inner.map.remove(&key);
                }
                KeyValueBatchOperation::DeletePrefix(prefix) => {
                    let keys = inner
                        .map
                        .range(prefix.clone()..)
                        .take_while(|(key, _)| key.starts_with(&prefix))
                        .map(|(key, _)| key.clone())
                        .collect::<Vec<_>>();
                    for key in keys {
                        inner.map.remove(&key);
                    }
                }
            }
        }
        finish_autocommit_write(&mut inner);
        Ok(())
    }
}

fn prepare_write(inner: &mut MemoryKeyValueState) -> StorageBackendResult<()> {
    if !inner.transactions.is_empty() && inner.transaction_read_only {
        return Err(StorageBackendError::Other(
            "cannot write in a read-only KeyValue transaction".into(),
        ));
    }
    if !inner.transactions.is_empty() {
        inner.transaction_written = true;
    }
    Ok(())
}

fn finish_autocommit_write(inner: &mut MemoryKeyValueState) {
    if inner.transactions.is_empty() {
        inner.change_version = inner.change_version.wrapping_add(1);
    }
}
