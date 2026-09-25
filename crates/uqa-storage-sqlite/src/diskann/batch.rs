//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native key and row translation preserves one evaluated atomic record batch.

use uqa_storage::mvcc::{SerializablePredicate, SerializableTransactionId};
use uqa_storage::read_control::StorageReadControl;
use uqa_storage::{KeyValueBatch, StorageBackendError, StorageBackendResult};

use super::encoding::Mapping;

pub(super) enum Owner<'a> {
    Scoped(&'a mut dyn KeyValueBatch),
    Owned(Box<dyn KeyValueBatch + 'a>),
}

pub(super) struct Batch<'a> {
    pub(super) inner: Owner<'a>,
    pub(super) mapping: Mapping,
    pub(super) control: StorageReadControl,
}

impl Owner<'_> {
    fn get(&self) -> &dyn KeyValueBatch {
        match self {
            Self::Scoped(batch) => &**batch,
            Self::Owned(batch) => &**batch,
        }
    }
    fn get_mut(&mut self) -> &mut dyn KeyValueBatch {
        match self {
            Self::Scoped(batch) => &mut **batch,
            Self::Owned(batch) => &mut **batch,
        }
    }
}

impl KeyValueBatch for Batch<'_> {
    fn serializable_participant(&self) -> Option<SerializableTransactionId> {
        self.inner.get().serializable_participant()
    }
    fn observe_serializable_write(
        &mut self,
        predicate: SerializablePredicate<'_>,
    ) -> StorageBackendResult<()> {
        self.inner.get_mut().observe_serializable_write(predicate)
    }
    fn observe_identifier(&mut self, namespace: &[u8], value: u64) -> StorageBackendResult<()> {
        self.inner.get_mut().observe_identifier(namespace, value)
    }
    fn inherit_identifiers(&mut self, from: &[u8], to: &[u8]) -> StorageBackendResult<()> {
        self.inner.get_mut().inherit_identifiers(from, to)
    }
    fn require_unchanged(&mut self, key: &[u8]) -> StorageBackendResult<()> {
        let key = self.mapping.key(key, &self.control)?;
        self.inner.get_mut().require_unchanged(&key)
    }
    fn fence_record(&mut self, key: &[u8]) -> StorageBackendResult<()> {
        let key = self.mapping.key(key, &self.control)?;
        self.inner.get_mut().fence_record(&key)
    }
    fn touch_marker(&mut self, key: &[u8], value: &[u8]) -> StorageBackendResult<()> {
        let record = self.mapping.record(key, value, &self.control)?;
        self.inner
            .get_mut()
            .touch_marker(record.key(), record.row())
    }
    fn put(&mut self, key: &[u8], value: &[u8]) -> StorageBackendResult<()> {
        let record = self.mapping.record(key, value, &self.control)?;
        self.inner.get_mut().put(record.key(), record.row())
    }
    fn delete(&mut self, key: &[u8]) -> StorageBackendResult<()> {
        let key = self.mapping.key(key, &self.control)?;
        self.inner.get_mut().delete(&key)
    }
    fn delete_prefix(&mut self, prefix: &[u8]) -> StorageBackendResult<()> {
        let prefix = self.mapping.prefix(prefix, &self.control)?;
        self.inner.get_mut().delete_prefix(&prefix)
    }
    fn commit(self: Box<Self>) -> StorageBackendResult<()> {
        match self.inner {
            Owner::Owned(batch) => batch.commit(),
            Owner::Scoped(_) => Err(StorageBackendError::Other(
                "scoped native batches complete with their original mutation".into(),
            )),
        }
    }
}
