//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Provider access to the selected catalog, canonical records and journal transaction.

use super::{invalid, SQLiteDiskANNCanonical};
use std::sync::Arc;
use uqa_core::DocId;
use uqa_storage::{
    diskann_index::{
        catalog::{DiskANNIndexResolver, DiskANNIndexScope},
        changes::{DiskANNPruneRequest, DiskANNPruneResult},
        format::DiskANNVectorVersion,
        maintenance::{DiskANNStatisticsPage, DiskANNStatisticsRequest},
        pages::DiskANNReadLimits,
        DiskANNLiveBinding, DiskANNPersistentBackend, DiskANNRetainedMutation,
        PersistentDiskANNHandle,
    },
    key_value::{KeyValueDiskANNPruner, KeyValueDiskANNSource},
    read_control::StorageReadControl,
    RelationIdentity, StorageBackendResult,
};

/// A session-bound index handle. Each snapshot retains the generation selected when it opens.
pub type SQLiteDiskANNHandle = PersistentDiskANNHandle<SQLiteDiskANNCanonical>;

impl SQLiteDiskANNCanonical {
    pub fn bind(
        self,
        index: RelationIdentity,
        resolver: Arc<dyn DiskANNIndexResolver + Send + Sync>,
        limits: DiskANNReadLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<SQLiteDiskANNHandle> {
        PersistentDiskANNHandle::bind(self, index, resolver, limits, control)
    }
}

impl DiskANNPersistentBackend for SQLiteDiskANNCanonical {
    const MISSING_PUBLICATION: &'static str = "live native index has no published generation";
    const MISSING_PARAMETERS: &'static str = "live native index has no bound parameters";
    const CHANGED_PARAMETERS: &'static str = "live native index parameters changed";

    fn dimensions(&self) -> u32 {
        self.index.dimensions
    }
    fn name_payload_bytes(&self) -> [usize; 2] {
        [
            self.index.table.unadmitted_payload_bytes(),
            self.index.field.unadmitted_payload_bytes(),
        ]
    }
    fn install_population(
        &self,
        source: &Self::Read,
        scope: &DiskANNIndexScope,
        selected: &KeyValueDiskANNSource,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        self.index
            .conn
            .with_native_write(|snapshot, batch| {
                source.initialize_population(scope, selected, snapshot, batch, control)?;
                Ok(())
            })?
            .ok_or_else(|| invalid("population initialization requires a native session"))
    }
    fn replace_bound(
        &self,
        live: &DiskANNLiveBinding<Self>,
        document: DocId,
        vectors: &[Vec<f32>],
    ) -> StorageBackendResult<DiskANNVectorVersion> {
        self.replace_guarded(document, vectors, &live.control, |snapshot, batch| {
            let canonical = &self.index;
            let binding = crate::catalog::DiskANNCatalogBinding::capture(
                snapshot,
                &canonical.table,
                &canonical.field,
                canonical.dimensions,
                &live.index,
                &live.control,
            )?;
            let current = binding.scope(snapshot, &*live.resolver, &live.control, &live.control)?;
            live.validate(&current, binding.parameters)?;
            let read = snapshot.record_read();
            let physical = crate::diskann::map_read(&read, snapshot.database)?;
            live.require_publication(&current, &physical)?;
            binding.require_current(&read, batch, &live.control)
        })
    }
    fn measure_changes(
        source: &Self::Read,
        coverage: &KeyValueDiskANNPruner,
        request: DiskANNStatisticsRequest,
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNStatisticsPage> {
        source.measure_changes(coverage, request, control)
    }
    fn prune_page(
        &self,
        source: &Self::Read,
        resolver: &dyn DiskANNIndexResolver,
        coverage: &KeyValueDiskANNPruner,
        request: DiskANNPruneRequest,
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNPruneResult> {
        self.index
            .conn
            .try_with_native_transaction_write(|current, batch| {
                Ok(source.prune_captured_changes(
                    resolver,
                    coverage,
                    (current, batch),
                    request,
                    control,
                )?)
            })?
            .ok_or_else(|| invalid("journal page requires the caller's transaction"))
    }
}
