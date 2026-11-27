//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Provider access to the selected catalog, canonical records and journal transaction.

use super::{invalid, KeyValueDiskANNCanonical};
use crate::key_value::catalog::diskann::Binding;
use crate::{
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
use std::sync::Arc;
use uqa_core::DocId;

/// A session-bound index handle. Each snapshot retains the generation selected when it opens.
pub type KeyValueDiskANNHandle = PersistentDiskANNHandle<KeyValueDiskANNCanonical>;

impl KeyValueDiskANNCanonical {
    pub fn bind(
        self,
        index: RelationIdentity,
        resolver: Arc<dyn DiskANNIndexResolver + Send + Sync>,
        limits: DiskANNReadLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<KeyValueDiskANNHandle> {
        PersistentDiskANNHandle::bind(self, index, resolver, limits, control)
    }
}

impl DiskANNPersistentBackend for KeyValueDiskANNCanonical {
    const MISSING_PUBLICATION: &'static str = "live index has no published generation";
    const MISSING_PARAMETERS: &'static str = "live index has no bound parameters";
    const CHANGED_PARAMETERS: &'static str = "live index parameters changed";

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
        self.index.store.with_mutation(&mut |read, batch| {
            source.initialize_population(scope, selected, read, batch, control)
        })
    }
    fn replace_bound(
        &self,
        live: &DiskANNLiveBinding<Self>,
        document: DocId,
        vectors: &[Vec<f32>],
    ) -> StorageBackendResult<DiskANNVectorVersion> {
        self.replace_guarded(document, vectors, &live.control, &mut |read, batch| {
            let canonical = &self.index;
            let binding = Binding::capture(
                read,
                &canonical.table,
                &canonical.field,
                canonical.dimensions,
                &live.index,
                &live.control,
            )?;
            let current = binding.scope(read, &*live.resolver, &live.control, &live.control)?;
            live.validate(&current, binding.parameters)?;
            live.require_publication(&current, read)?;
            binding.require_current(read, batch, &live.control)
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
        let mut result = None;
        self.index
            .store
            .try_with_transaction_mutation(&mut |read, batch| {
                result = Some(source.prune_captured_changes(
                    resolver,
                    coverage,
                    (read, batch),
                    request,
                    control,
                )?);
                Ok(())
            })?;
        result.ok_or_else(|| invalid("journal page requires the caller's transaction"))
    }
}
