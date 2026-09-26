//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compound native reads keep the original committed/private boundary and query allowance.

use std::{ops::Deref, sync::Arc};

use uqa_core::memory::{BudgetedVec, MemoryReservation};
use uqa_storage::key_value::{KeyValueRead, KeyValueReadRevision};
use uqa_storage::mvcc::VersionError;
use uqa_storage::read_control::{
    KeyReadVisitor, KeyValueReadVisitor, StorageReadControl, ValueReadVisitor,
};
use uqa_storage::{KeyValueStore, StorageBackendResult};

use super::encoding::{invalid, Mapping};

pub(super) struct Read<R> {
    pub(super) inner: R,
    pub(super) mapping: Mapping,
    pub(super) _memory: Option<MemoryReservation>,
}

impl<R> KeyValueRead for Read<R>
where
    R: Deref,
    R::Target: KeyValueRead,
{
    fn control(&self) -> &StorageReadControl {
        self.inner.control()
    }

    fn revision(&self, prefixes: &[&[u8]]) -> StorageBackendResult<KeyValueReadRevision> {
        self.with_prefixes(prefixes, |encoded| self.inner.revision(encoded))
    }

    fn record_revision(&self, key: &[u8]) -> StorageBackendResult<Option<KeyValueReadRevision>> {
        self.inner
            .record_revision(&self.mapping.key(key, self.control())?)
    }

    fn retained_source(
        &self,
        key: &[u8],
    ) -> StorageBackendResult<Option<Arc<dyn KeyValueRead + Send + Sync>>> {
        self.inner
            .retained_source(&self.mapping.key(key, self.control())?)
    }

    fn retain(
        &self,
        prefixes: &[&[u8]],
    ) -> StorageBackendResult<Arc<dyn KeyValueRead + Send + Sync>> {
        self.with_prefixes(prefixes, |encoded| {
            let memory = self
                .control()
                .memory()
                .reserve(std::mem::size_of::<Read<Arc<dyn KeyValueRead + Send + Sync>>>())?;
            Ok(Arc::new(Read {
                inner: self.inner.retain(encoded)?,
                mapping: self.mapping,
                _memory: Some(memory),
            }) as Arc<dyn KeyValueRead + Send + Sync>)
        })
    }

    fn visit_value(
        &self,
        key: &[u8],
        visit: &mut ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.visit_value_bounded(key, usize::MAX, self.control(), visit)
    }

    fn visit_value_budgeted(
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
        let encoded = self.mapping.key(key, control)?;
        let limit = crate::mvcc::native::binary_pair_limit(key.len(), max_bytes)
            .map_err(VersionError::into_storage_error)?;
        self.inner
            .visit_value_bounded(&encoded, limit, control, &mut |value| {
                let Some(value) = value else {
                    return visit(None);
                };
                self.mapping
                    .visit_value(&encoded, value, control, &mut |logical, payload| {
                        if logical != key {
                            return Err(invalid());
                        }
                        control.check_value_size(payload.len(), max_bytes)?;
                        visit(Some(payload))
                    })
            })
    }

    fn visit_prefix(
        &self,
        prefix: &[u8],
        visit: &mut KeyValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.visit_prefix_after(prefix, None, usize::MAX, self.control(), visit)
    }

    fn visit_prefix_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut KeyValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        let encoded = self.mapping.prefix(prefix, control)?;
        let after = after
            .map(|key| self.mapping.key(key, control))
            .transpose()?;
        self.inner.visit_prefix_after(
            &encoded,
            after.as_deref(),
            limit,
            control,
            &mut |key, value| self.mapping.visit_value(key, value, control, visit),
        )
    }

    fn visit_keys_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut KeyReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        let encoded = self.mapping.prefix(prefix, control)?;
        let after = after
            .map(|key| self.mapping.key(key, control))
            .transpose()?;
        self.inner
            .visit_keys_after(&encoded, after.as_deref(), limit, control, &mut |key| {
                self.mapping.visit_key(key, control, visit)
            })
    }

    fn contains_prefix_budgeted(
        &self,
        prefix: &[u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<bool> {
        let encoded = self.mapping.prefix(prefix, control)?;
        self.inner.contains_prefix_budgeted(&encoded, control)
    }
}

impl<R> Read<R>
where
    R: Deref,
    R::Target: KeyValueRead,
{
    fn with_prefixes<T>(
        &self,
        prefixes: &[&[u8]],
        operation: impl FnOnce(&[&[u8]]) -> StorageBackendResult<T>,
    ) -> StorageBackendResult<T> {
        let mut encoded = BudgetedVec::new(self.control().memory());
        for prefix in prefixes {
            encoded.push(self.mapping.prefix(prefix, self.control())?)?;
        }
        let mut borrowed = BudgetedVec::new(self.control().memory());
        for prefix in &*encoded {
            borrowed.push(&prefix[..])?;
        }
        operation(&borrowed)
    }
}

pub(super) fn collect_pairs(
    store: &dyn KeyValueStore,
    prefix: &[u8],
    after: Option<&[u8]>,
    limit: usize,
    control: &StorageReadControl,
) -> StorageBackendResult<Vec<(Vec<u8>, Vec<u8>)>> {
    let mut result = BudgetedVec::new(control.memory());
    store.visit_prefix_after(prefix, after, limit, control, &mut |key, value| {
        let mut a = BudgetedVec::new(control.memory());
        a.extend_from_slice(key)?;
        let mut b = BudgetedVec::new(control.memory());
        b.extend_from_slice(value)?;
        result.push((a, b))?;
        Ok(())
    })?;
    // Legacy owned-return methods transfer bytes to the caller; controlled visitors above retain their original reservations throughout callbacks.
    let (result, _reservation) = result.into_parts();
    Ok(result
        .into_iter()
        .map(|(key, value)| (key.into_parts().0, value.into_parts().0))
        .collect())
}

pub(super) fn collect_keys(
    store: &dyn KeyValueStore,
    prefix: &[u8],
    after: Option<&[u8]>,
    limit: usize,
    control: &StorageReadControl,
) -> StorageBackendResult<Vec<Vec<u8>>> {
    let mut result = BudgetedVec::new(control.memory());
    store.with_read_view(&mut |read| {
        read.visit_keys_after(prefix, after, limit, control, &mut |key| {
            let mut bytes = BudgetedVec::new(control.memory());
            bytes.extend_from_slice(key)?;
            result.push(bytes)?;
            Ok(())
        })
    })?;
    let (result, _reservation) = result.into_parts();
    Ok(result.into_iter().map(|key| key.into_parts().0).collect())
}
