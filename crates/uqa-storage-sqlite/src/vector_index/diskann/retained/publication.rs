//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog guards and generation selection share the caller's native record mutation.

use super::{invalid, Family, Identity, RetainedSQLiteDiskANNCanonical};
use crate::mvcc::native::NativeSnapshot;
use rusqlite::types::ValueRef;
use uqa_storage::{
    diskann_index::{
        build::DiskANNCanonicalCoverage, catalog::DiskANNIndexResolver, format::DiskANNGeneration,
        DiskANNRetainedMutation,
    },
    key_value::{KeyValueDiskANNSource, KeyValueRead},
    mvcc::VersionError,
    read_control::StorageReadControl,
    KeyValueBatch, StorageBackendResult,
};

impl RetainedSQLiteDiskANNCanonical {
    pub(crate) fn retire_generation(
        &self,
        resolver: &dyn DiskANNIndexResolver,
        current: &NativeSnapshot,
        batch: &mut dyn KeyValueBatch,
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNGeneration> {
        DiskANNRetainedMutation::retire_generation(self, resolver, current, batch, control)
    }

    pub(crate) fn publish_generation(
        coverage: &DiskANNCanonicalCoverage<Self>,
        resolver: &dyn DiskANNIndexResolver,
        sealed: &KeyValueDiskANNSource,
        current: &NativeSnapshot,
        batch: &mut dyn KeyValueBatch,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        <Self as DiskANNRetainedMutation<_>>::publish_generation(
            coverage, resolver, sealed, current, batch, control,
        )
    }
}

impl uqa_storage::diskann_index::DiskANNMutationView for NativeSnapshot {
    fn with_generation_read<T>(
        &self,
        visit: impl FnOnce(&dyn KeyValueRead) -> StorageBackendResult<T>,
    ) -> StorageBackendResult<T> {
        let records = self.record_read();
        let physical = crate::diskann::map_read(&records, self.database)?;
        visit(&physical)
    }
    fn with_generation_batch<T>(
        &self,
        batch: &mut dyn KeyValueBatch,
        visit: impl FnOnce(&mut dyn KeyValueBatch) -> StorageBackendResult<T>,
    ) -> StorageBackendResult<T> {
        let mut mapped = crate::diskann::map_batch(batch, self.database, &self.control)?;
        visit(&mut mapped)
    }
}

impl DiskANNRetainedMutation<NativeSnapshot> for RetainedSQLiteDiskANNCanonical {
    fn guard_catalog(
        &self,
        current: &NativeSnapshot,
        batch: &mut dyn KeyValueBatch,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        self.require_current_index(&current.record_read(), batch, control)
    }
    fn with_canonical_records<T>(
        &self,
        current: &NativeSnapshot,
        missing_owner: &'static str,
        control: &StorageReadControl,
        visit: impl FnOnce(&dyn KeyValueRead, &dyn KeyValueRead, &[&[u8]]) -> StorageBackendResult<T>,
    ) -> StorageBackendResult<T> {
        let owner = self.owner.ok_or_else(|| invalid(missing_owner))?;
        let prefix = |family| {
            Identity::new(family, owner)
                .and_then(|identity| {
                    identity.encode_prefix(&[ValueRef::Text(&self.field)], control)
                })
                .map_err(VersionError::into_storage_error)
        };
        let vectors = prefix(Family::Vectors)?;
        let origins = prefix(Family::VectorOrigins)?;
        visit(
            &self.snapshot.record_read(),
            &current.record_read(),
            &[&vectors, &origins],
        )
    }
    fn retire_population(
        &self,
        generation: DiskANNGeneration,
        batch: &mut dyn KeyValueBatch,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        Self::retire_population(self, generation, batch, control)
    }
    fn publish_population(
        &self,
        sealed: &KeyValueDiskANNSource,
        batch: &mut dyn KeyValueBatch,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        Self::publish_population(self, sealed, batch, control)
    }
}
