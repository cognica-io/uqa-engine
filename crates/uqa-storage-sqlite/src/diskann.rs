//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Private native record translation for Storage's shared physical generation lifecycle.

use std::sync::Arc;

use uqa_core::{memory::MemoryReservation, CancellationToken};
use uqa_storage::key_value::{KeyValueMutation, KeyValueReadScope, KeyValueVersionedMutation};
use uqa_storage::mvcc::{DatabaseId, IdentifierAllocator, SerializableSession};
use uqa_storage::read_control::{KeyValueReadVisitor, StorageReadControl, ValueReadVisitor};
use uqa_storage::{
    KeyValueBatch, KeyValueStore, PersistentStorageIdentity, StorageBackendResult,
    StorageEncryptionKey, StorageSessionAffinity, StorageTransactionModel,
};

mod batch;
mod encoding;
mod read;
#[cfg(test)]
mod tests;

use batch::{Batch, Owner};
use encoding::{invalid, Mapping};
use read::Read;

pub(crate) fn map_read(
    inner: &dyn uqa_storage::key_value::KeyValueRead,
    namespace: DatabaseId,
) -> StorageBackendResult<impl uqa_storage::key_value::KeyValueRead + '_> {
    Ok(Read {
        inner,
        mapping: Mapping::new(namespace)?,
        _memory: None,
    })
}

pub(crate) fn map_batch<'a>(
    inner: &'a mut dyn KeyValueBatch,
    namespace: DatabaseId,
    control: &StorageReadControl,
) -> StorageBackendResult<impl KeyValueBatch + 'a> {
    Ok(Batch {
        inner: Owner::Scoped(inner),
        mapping: Mapping::new(namespace)?,
        control: control.clone(),
    })
}

pub(crate) struct Records {
    inner: Arc<dyn KeyValueStore>,
    mapping: Mapping,
    encryption: Option<StorageEncryptionKey>,
    control: StorageReadControl,
    _memory: MemoryReservation,
}

impl Records {
    pub(crate) fn new(
        inner: Arc<dyn KeyValueStore>,
        namespace: DatabaseId,
        encryption: Option<StorageEncryptionKey>,
    ) -> StorageBackendResult<Self> {
        let control = inner.retention_control().ok_or_else(invalid)?;
        Ok(Self {
            _memory: control.memory().reserve(std::mem::size_of::<Self>())?,
            inner,
            mapping: Mapping::new(namespace)?,
            encryption,
            control,
        })
    }

    fn wrap(&self, inner: Arc<dyn KeyValueStore>) -> StorageBackendResult<Arc<dyn KeyValueStore>> {
        let control = inner.retention_control().ok_or_else(invalid)?;
        Ok(Arc::new(Self {
            _memory: control.memory().reserve(std::mem::size_of::<Self>())?,
            inner,
            mapping: self.mapping,
            encryption: self.encryption.clone(),
            control,
        }))
    }
}

impl KeyValueStore for Records {
    fn resource_leases(&self) -> Option<&dyn uqa_storage::mvcc::ResourceLeaseProvider> {
        self.inner.resource_leases()
    }

    fn auxiliary_encryption_key(&self) -> Option<StorageEncryptionKey> {
        self.encryption.clone()
    }
    fn retention_control(&self) -> Option<StorageReadControl> {
        Some(self.control.clone())
    }
    fn notification_publications(
        &self,
    ) -> Option<&dyn uqa_storage::notifications::NotificationPublicationStore> {
        self.inner.notification_publications()
    }
    fn serializable_session(&self) -> Option<&dyn SerializableSession> {
        self.inner.serializable_session()
    }
    fn identifier_allocator(&self) -> Option<&dyn IdentifierAllocator> {
        self.inner.identifier_allocator()
    }
    fn transaction_model(&self) -> StorageTransactionModel {
        self.inner.transaction_model()
    }
    fn transaction_affinity(&self) -> Option<StorageSessionAffinity> {
        self.inner.transaction_affinity()
    }
    fn storage_identity(&self) -> StorageBackendResult<Option<PersistentStorageIdentity>> {
        self.inner.storage_identity()
    }
    fn write_cancellation(&self) -> Option<CancellationToken> {
        self.inner.write_cancellation()
    }
    fn vacuum(&self) -> StorageBackendResult<()> {
        self.inner.vacuum()
    }

    fn open_session(&self) -> StorageBackendResult<Arc<dyn KeyValueStore>> {
        self.wrap(self.inner.open_session()?)
    }
    fn open_session_with_cancellation(
        &self,
        cancellation: &CancellationToken,
    ) -> StorageBackendResult<Arc<dyn KeyValueStore>> {
        self.wrap(self.inner.open_session_with_cancellation(cancellation)?)
    }
    fn open_retained_read_session(
        &self,
        cancellation: &CancellationToken,
    ) -> StorageBackendResult<Arc<dyn KeyValueStore>> {
        self.wrap(self.inner.open_retained_read_session(cancellation)?)
    }

    fn with_read_view(&self, read: &mut KeyValueReadScope<'_>) -> StorageBackendResult<()> {
        self.inner.with_read_view(&mut |inner| {
            read(&Read {
                inner,
                mapping: self.mapping,
                _memory: None,
            })
        })
    }

    fn with_mutation(&self, mutate: &mut KeyValueMutation<'_>) -> StorageBackendResult<()> {
        self.inner.with_mutation(&mut |read, batch| {
            mutate(
                &Read {
                    inner: read,
                    mapping: self.mapping,
                    _memory: None,
                },
                &mut Batch {
                    inner: Owner::Scoped(batch),
                    mapping: self.mapping,
                    control: read.control().clone(),
                },
            )
        })
    }

    fn with_versioned_mutation(
        &self,
        mutate: &mut KeyValueVersionedMutation<'_>,
    ) -> StorageBackendResult<()> {
        self.inner
            .with_versioned_mutation(&mut |origin, read, batch| {
                mutate(
                    origin,
                    &Read {
                        inner: read,
                        mapping: self.mapping,
                        _memory: None,
                    },
                    &mut Batch {
                        inner: Owner::Scoped(batch),
                        mapping: self.mapping,
                        control: read.control().clone(),
                    },
                )
            })
    }

    fn get(&self, key: &[u8]) -> StorageBackendResult<Option<Vec<u8>>> {
        let mut result = None;
        self.with_read_view(&mut |read| {
            result = read.get(key)?.map(|bytes| bytes.into_parts().0);
            Ok(())
        })?;
        Ok(result)
    }

    fn contains_key(&self, key: &[u8]) -> StorageBackendResult<bool> {
        self.inner
            .contains_key(&self.mapping.key(key, &self.control)?)
    }

    fn contains_prefix_budgeted(
        &self,
        prefix: &[u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<bool> {
        let mut result = false;
        self.with_read_view(&mut |read| {
            result = read.contains_prefix_budgeted(prefix, control)?;
            Ok(())
        })?;
        Ok(result)
    }

    fn visit_value(
        &self,
        key: &[u8],
        control: &StorageReadControl,
        visit: &mut ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.visit_value_bounded(key, usize::MAX, control, visit)
    }

    fn visit_value_bounded(
        &self,
        key: &[u8],
        max_bytes: usize,
        control: &StorageReadControl,
        visit: &mut ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.with_read_view(&mut |read| read.visit_value_bounded(key, max_bytes, control, visit))
    }

    fn visit_prefix_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut KeyValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.with_read_view(&mut |read| {
            read.visit_prefix_after(prefix, after, limit, control, visit)
        })
    }

    fn put(&self, key: &[u8], value: &[u8]) -> StorageBackendResult<()> {
        self.with_mutation(&mut |_, batch| batch.put(key, value))
    }
    fn delete(&self, key: &[u8]) -> StorageBackendResult<()> {
        self.with_mutation(&mut |_, batch| batch.delete(key))
    }
    fn delete_prefix(&self, prefix: &[u8]) -> StorageBackendResult<usize> {
        let mut count = 0usize;
        self.with_mutation(&mut |read, batch| {
            read.visit_keys_after(prefix, None, usize::MAX, read.control(), &mut |_| {
                count = count
                    .checked_add(1)
                    .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
                Ok(())
            })?;
            batch.delete_prefix(prefix)
        })?;
        Ok(count)
    }

    fn scan_prefix(&self, prefix: &[u8]) -> StorageBackendResult<Vec<(Vec<u8>, Vec<u8>)>> {
        self.scan_prefix_after(prefix, None, usize::MAX)
    }
    fn scan_prefix_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
    ) -> StorageBackendResult<Vec<(Vec<u8>, Vec<u8>)>> {
        read::collect_pairs(self, prefix, after, limit, &self.control)
    }
    fn scan_prefix_keys_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
    ) -> StorageBackendResult<Vec<Vec<u8>>> {
        read::collect_keys(self, prefix, after, limit, &self.control)
    }
    fn first_prefix_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
    ) -> StorageBackendResult<Option<(Vec<u8>, Vec<u8>)>> {
        Ok(self.scan_prefix_after(prefix, after, 1)?.pop())
    }

    fn batch(&self) -> Box<dyn KeyValueBatch + '_> {
        Box::new(Batch {
            inner: Owner::Owned(self.inner.batch()),
            mapping: self.mapping,
            control: self.control.clone(),
        })
    }

    fn begin_transaction(&self) -> StorageBackendResult<()> {
        self.inner.begin_transaction()
    }
    fn begin_read_transaction(&self) -> StorageBackendResult<()> {
        self.inner.begin_read_transaction()
    }
    fn begin_upgradeable_transaction(&self) -> StorageBackendResult<()> {
        self.inner.begin_upgradeable_transaction()
    }
    fn in_transaction(&self) -> bool {
        self.inner.in_transaction()
    }
    fn transaction_has_written(&self) -> StorageBackendResult<bool> {
        self.inner.transaction_has_written()
    }
    fn change_version(&self) -> StorageBackendResult<Option<u64>> {
        self.inner.change_version()
    }
    fn change_version_monitor_is_nonblocking(&self) -> StorageBackendResult<bool> {
        self.inner.change_version_monitor_is_nonblocking()
    }
    fn pin_transaction_snapshot(&self) -> StorageBackendResult<()> {
        self.inner.pin_transaction_snapshot()
    }
    fn refresh_transaction_snapshot(
        &self,
        cancellation: &CancellationToken,
    ) -> StorageBackendResult<()> {
        self.inner.refresh_transaction_snapshot(cancellation)
    }
    fn commit_transaction(&self) -> StorageBackendResult<()> {
        self.inner.commit_transaction()
    }
    fn rollback_transaction(&self) -> StorageBackendResult<()> {
        self.inner.rollback_transaction()
    }
    fn savepoint(&self, name: &str) -> StorageBackendResult<()> {
        self.inner.savepoint(name)
    }
    fn release_savepoint(&self, name: &str) -> StorageBackendResult<()> {
        self.inner.release_savepoint(name)
    }
    fn rollback_to_savepoint(&self, name: &str) -> StorageBackendResult<()> {
        self.inner.rollback_to_savepoint(name)
    }
}
