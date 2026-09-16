//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::memory::BudgetedVec;

use crate::mvcc::commit::RecordWriteKind;
use crate::mvcc::graph::OwnedGraphMutation;
use crate::mvcc::VersionError;
use crate::{KeyValueBatch, StorageBackendResult};

use super::VersionedKeyValueStore;

enum Operation {
    Put(BudgetedVec<u8>, BudgetedVec<u8>),
    Delete(BudgetedVec<u8>),
    DeletePrefix(BudgetedVec<u8>),
    Graph(OwnedGraphMutation),
    GraphCache {
        key: BudgetedVec<u8>,
        value: Option<BudgetedVec<u8>>,
        kind: RecordWriteKind,
    },
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
    fn cache(
        &mut self,
        key: &[u8],
        value: Option<&[u8]>,
        kind: RecordWriteKind,
    ) -> StorageBackendResult<()> {
        self.operations.push(Operation::GraphCache {
            key: self.copy(key)?,
            value: value.map(|value| self.copy(value)).transpose()?,
            kind,
        })?;
        Ok(())
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
    fn graph_mutation(
        &mut self,
        mutation: crate::mvcc::GraphMutation<'_>,
    ) -> StorageBackendResult<()> {
        self.operations.push(Operation::Graph(
            OwnedGraphMutation::retain(mutation, &self.store.control)
                .map_err(VersionError::into_storage_error)?,
        ))?;
        Ok(())
    }
    fn preview_graph_invalidation(
        &mut self,
        key: &[u8],
        value: Option<&[u8]>,
    ) -> StorageBackendResult<()> {
        self.cache(key, value, RecordWriteKind::GraphPreview)
    }
    fn replace_graph_cache(
        &mut self,
        key: &[u8],
        value: Option<&[u8]>,
    ) -> StorageBackendResult<()> {
        self.cache(key, value, RecordWriteKind::GraphCache)
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
                    Operation::Graph(mutation) => {
                        transaction.graph_mutation(mutation)?;
                    }
                    Operation::GraphCache { key, value, kind } => {
                        transaction.write_record(
                            key,
                            value.as_deref(),
                            *kind,
                            &self.store.control,
                        )?;
                    }
                }
            }
            Ok::<_, VersionError>(())
        })
    }
}
