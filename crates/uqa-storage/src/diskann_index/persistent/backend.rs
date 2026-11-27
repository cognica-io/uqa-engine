//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Transactions and physical records supplied by persistent vector storage adapters.

use super::{invalid, DiskANNBoundRead, DiskANNLifecycle, DiskANNLiveBinding};
use crate::{
    diskann_index::{
        catalog::{DiskANNIndexResolver, DiskANNIndexScope},
        changes::{DiskANNPruneRequest, DiskANNPruneResult},
        format::DiskANNVectorVersion,
        maintenance::{DiskANNStatisticsPage, DiskANNStatisticsRequest},
        pages::DiskANNPageSource,
        DiskANNCanonicalRead,
    },
    key_value::{KeyValueDiskANNPruner, KeyValueDiskANNSource},
    read_control::StorageReadControl,
    RelationIdentity, StorageBackendResult,
};
use uqa_core::DocId;

/// Captures a fixed canonical view and performs guarded writes on its provider's bound transaction.
pub trait DiskANNPersistentBackend: DiskANNLifecycle {
    const MISSING_PUBLICATION: &'static str;
    const MISSING_PARAMETERS: &'static str;
    const CHANGED_PARAMETERS: &'static str;

    fn dimensions(&self) -> u32;
    fn name_payload_bytes(&self) -> [usize; 2];
    fn initialize_populations(
        &self,
        index: &RelationIdentity,
        resolver: &dyn DiskANNIndexResolver,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        let source = self.retain_for_index(index, control)?;
        let selected = source
            .selected_source(resolver, control)?
            .ok_or_else(|| invalid("population initialization has no selected generation"))?;
        if source
            .population_counts(selected.generation(), control)?
            .is_some()
        {
            return Ok(());
        }
        let scope = source.index_scope(resolver, control)?;
        self.install_population(&source, &scope, &selected, control)
    }
    fn install_population(
        &self,
        source: &Self::Read,
        scope: &DiskANNIndexScope,
        selected: &KeyValueDiskANNSource,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()>;
    fn replace_bound(
        &self,
        binding: &DiskANNLiveBinding<Self>,
        document: DocId,
        vectors: &[Vec<f32>],
    ) -> StorageBackendResult<DiskANNVectorVersion>;
    fn measure_changes(
        source: &Self::Read,
        coverage: &KeyValueDiskANNPruner,
        request: DiskANNStatisticsRequest,
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNStatisticsPage>;
    fn prune_page(
        &self,
        source: &Self::Read,
        resolver: &dyn DiskANNIndexResolver,
        coverage: &KeyValueDiskANNPruner,
        request: DiskANNPruneRequest,
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNPruneResult>;
}
