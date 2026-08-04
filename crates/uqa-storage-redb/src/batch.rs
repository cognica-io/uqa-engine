//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_storage::{KeyValueBatch, StorageBackendResult};

use crate::store::RedbKeyValueStore;

pub(crate) enum BatchOperation {
    Put(Vec<u8>, Vec<u8>),
    Delete(Vec<u8>),
    DeletePrefix(Vec<u8>),
}

pub(crate) struct RedbBatch<'a> {
    store: &'a RedbKeyValueStore,
    operations: Vec<BatchOperation>,
}

impl<'a> RedbBatch<'a> {
    pub(crate) fn new(store: &'a RedbKeyValueStore) -> Self {
        Self {
            store,
            operations: Vec::new(),
        }
    }
}

impl KeyValueBatch for RedbBatch<'_> {
    fn put(&mut self, key: &[u8], value: &[u8]) -> StorageBackendResult<()> {
        self.operations
            .push(BatchOperation::Put(key.to_vec(), value.to_vec()));
        Ok(())
    }

    fn delete(&mut self, key: &[u8]) -> StorageBackendResult<()> {
        self.operations.push(BatchOperation::Delete(key.to_vec()));
        Ok(())
    }

    fn delete_prefix(&mut self, prefix: &[u8]) -> StorageBackendResult<()> {
        self.operations
            .push(BatchOperation::DeletePrefix(prefix.to_vec()));
        Ok(())
    }

    fn commit(self: Box<Self>) -> StorageBackendResult<()> {
        self.store.commit_batch(&self.operations)
    }
}
