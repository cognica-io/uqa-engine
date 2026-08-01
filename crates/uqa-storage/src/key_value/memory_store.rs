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
    savepoints: BTreeMap<String, BTreeMap<Vec<u8>, Vec<u8>>>,
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
        self.inner.lock().map.insert(key.to_vec(), value.to_vec());
        Ok(())
    }

    fn delete(&self, key: &[u8]) -> StorageBackendResult<()> {
        self.inner.lock().map.remove(key);
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

    fn first_prefix_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
    ) -> StorageBackendResult<Option<(Vec<u8>, Vec<u8>)>> {
        use std::ops::Bound::{Excluded, Included, Unbounded};

        let inner = self.inner.lock();
        let lower = after.map_or_else(
            || Included(prefix.to_vec()),
            |after| Excluded(after.to_vec()),
        );
        Ok(inner
            .map
            .range((lower, Unbounded))
            .next()
            .filter(|(key, _)| key.starts_with(prefix))
            .map(|(key, value)| (key.clone(), value.clone())))
    }

    fn delete_prefix(&self, prefix: &[u8]) -> StorageBackendResult<usize> {
        let mut inner = self.inner.lock();
        let keys = inner
            .map
            .range(prefix.to_vec()..)
            .take_while(|(key, _)| key.starts_with(prefix))
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in &keys {
            inner.map.remove(key);
        }
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
        let snapshot = inner.map.clone();
        inner.transactions.push(snapshot);
        Ok(())
    }

    fn commit_transaction(&self) -> StorageBackendResult<()> {
        let mut inner = self.inner.lock();
        inner.transactions.pop().ok_or_else(|| {
            StorageBackendError::Other("no open KeyValue transaction to commit".into())
        })?;
        inner.savepoints.clear();
        Ok(())
    }

    fn rollback_transaction(&self) -> StorageBackendResult<()> {
        let mut inner = self.inner.lock();
        let snapshot = inner.transactions.pop().ok_or_else(|| {
            StorageBackendError::Other("no open KeyValue transaction to roll back".into())
        })?;
        inner.map = snapshot;
        inner.savepoints.clear();
        Ok(())
    }

    fn savepoint(&self, name: &str) -> StorageBackendResult<()> {
        let mut inner = self.inner.lock();
        let snapshot = inner.map.clone();
        inner.savepoints.insert(name.to_string(), snapshot);
        Ok(())
    }

    fn release_savepoint(&self, name: &str) -> StorageBackendResult<()> {
        self.inner.lock().savepoints.remove(name);
        Ok(())
    }

    fn rollback_to_savepoint(&self, name: &str) -> StorageBackendResult<()> {
        let mut inner = self.inner.lock();
        let snapshot = inner
            .savepoints
            .get(name)
            .cloned()
            .ok_or_else(|| StorageBackendError::Other(format!("unknown savepoint `{name}`")))?;
        inner.map = snapshot;
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
        Ok(())
    }
}
