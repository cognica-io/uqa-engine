//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::memory::BudgetedVec;

use crate::mvcc::VersionError;
use crate::{KeyValueBatch, StorageBackendResult};

use super::VersionedKeyValueStore;

enum Operation {
    Put(BudgetedVec<u8>, BudgetedVec<u8>),
    Delete(BudgetedVec<u8>),
    DeletePrefix(BudgetedVec<u8>),
}

pub(super) struct Batch<'a> {
    store: &'a VersionedKeyValueStore,
    operations: BudgetedVec<Operation>,
}

impl<'a> Batch<'a> {
    pub(super) fn new(store: &'a VersionedKeyValueStore) -> Self {
        Self {
            store,
            operations: BudgetedVec::new(store.control.memory()),
        }
    }
    fn copy(&self, bytes: &[u8]) -> StorageBackendResult<BudgetedVec<u8>> {
        let mut owned = BudgetedVec::new(self.store.control.memory());
        owned.extend_from_slice(bytes)?;
        Ok(owned)
    }
}

impl KeyValueBatch for Batch<'_> {
    fn put(&mut self, key: &[u8], value: &[u8]) -> StorageBackendResult<()> {
        self.operations
            .push(Operation::Put(self.copy(key)?, self.copy(value)?))?;
        Ok(())
    }
    fn delete(&mut self, key: &[u8]) -> StorageBackendResult<()> {
        self.operations.push(Operation::Delete(self.copy(key)?))?;
        Ok(())
    }
    fn delete_prefix(&mut self, prefix: &[u8]) -> StorageBackendResult<()> {
        self.operations
            .push(Operation::DeletePrefix(self.copy(prefix)?))?;
        Ok(())
    }
    fn commit(self: Box<Self>) -> StorageBackendResult<()> {
        self.store.write(|transaction| {
            for operation in self.operations.iter() {
                match operation {
                    Operation::Put(key, value) => {
                        transaction.replace(key, Some(value), &self.store.control)?;
                    }
                    Operation::Delete(key) => {
                        transaction.replace(key, None, &self.store.control)?;
                    }
                    Operation::DeletePrefix(prefix) => {
                        transaction.delete_prefix(prefix, &self.store.control)?;
                    }
                }
            }
            Ok::<_, VersionError>(())
        })
    }
}
