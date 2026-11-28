//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::RetainedDiskANNCanonical;
use crate::diskann_index::{
    build::DiskANNCanonicalCoverage, catalog::DiskANNIndexResolver, format::DiskANNGeneration,
    DiskANNRetainedMutation,
};
use crate::key_value::{KeyValueDiskANNSource, KeyValueRead};
use crate::{read_control::StorageReadControl, KeyValueBatch, StorageBackendResult};

impl RetainedDiskANNCanonical {
    /// Retire this captured head before its SQL catalog row is removed. A competing publication or definition invalidates the operation; retained snapshots remain readable.
    pub fn retire_generation(
        &self,
        resolver: &dyn DiskANNIndexResolver,
        read: &dyn KeyValueRead,
        batch: &mut dyn KeyValueBatch,
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNGeneration> {
        DiskANNRetainedMutation::retire_generation(self, resolver, read, batch, control)
    }

    /// Publish a completed build in the supplied caller mutation. The read and batch must belong to the same `with_mutation` callback; this method never completes that transaction. The SQL lifecycle must supersede the effect if later private DDL invalidates the index.
    pub fn publish_generation(
        coverage: &DiskANNCanonicalCoverage<Self>,
        resolver: &dyn DiskANNIndexResolver,
        sealed: &KeyValueDiskANNSource,
        read: &dyn KeyValueRead,
        batch: &mut dyn KeyValueBatch,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        <Self as DiskANNRetainedMutation<_>>::publish_generation(
            coverage, resolver, sealed, read, batch, control,
        )
    }
}

impl<'view> DiskANNRetainedMutation<dyn KeyValueRead + 'view> for RetainedDiskANNCanonical {
    fn guard_catalog(
        &self,
        current: &(dyn KeyValueRead + 'view),
        batch: &mut dyn KeyValueBatch,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        self.require_current_index(current, batch, control)
    }
    fn with_canonical_records<T>(
        &self,
        current: &(dyn KeyValueRead + 'view),
        _: &'static str,
        _: &StorageReadControl,
        visit: impl FnOnce(&dyn KeyValueRead, &dyn KeyValueRead, &[&[u8]]) -> StorageBackendResult<T>,
    ) -> StorageBackendResult<T> {
        visit(&*self.read, current, &[&self.vectors, &self.origins])
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
