//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generation census is a writable lifecycle operation. Metadata reads only decode one fixed header on their retained view.

use super::super::KeyValueDiskANNPopulationRecords as Layout;
use super::super::{invalid, KeyValueDiskANNCanonical, RetainedDiskANNCanonical};
use crate::diskann_index::{
    format::{DiskANNGeneration, DiskANNManifest, DiskANNOriginLayout},
    pages::DiskANNPageSource,
    DiskANNCanonicalCounts, DiskANNCanonicalRead, DiskANNPopulationState,
};
use crate::key_value::{publication, KeyValueDiskANNSource};
use crate::mvcc::VersionError;
use crate::{read_control::StorageReadControl, KeyValueBatch, StorageBackendResult};

impl RetainedDiskANNCanonical {
    pub(in crate::key_value) fn read_population(
        &self,
        generation: DiskANNGeneration,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNCanonicalCounts>> {
        self.check_control(control)?;
        let key = Layout::header_key(&self.vectors, generation, control)
            .map_err(VersionError::into_storage_error)?;
        let bytes = crate::key_value::diskann::state::fixed::<
            { DiskANNPopulationState::ENCODED_BYTES },
        >(control, |visit| {
            self.read.visit_value_bounded(
                &key,
                DiskANNPopulationState::ENCODED_BYTES,
                control,
                visit,
            )
        })?;
        let state = bytes
            .map(|bytes| DiskANNPopulationState::decode(&bytes, generation, self.dimensions))
            .transpose()?;
        self.check_control(control)?;
        Ok(state.map(DiskANNPopulationState::counts))
    }

    pub(in crate::key_value) fn publish_population(
        &self,
        sealed: &KeyValueDiskANNSource,
        batch: &mut dyn KeyValueBatch,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        let maximum =
            DiskANNManifest::MAX_ENCODED_BYTES.max(DiskANNOriginLayout::MAX_ENCODED_BYTES);
        let origins = sealed.origin_reader(maximum, control)?;
        let generation = origins.manifest().input().generation;
        let key = Layout::header_key(&self.vectors, generation, control)
            .map_err(VersionError::into_storage_error)?;
        let template = DiskANNPopulationState::from_counts(
            generation,
            self.dimensions,
            DiskANNCanonicalCounts::default(),
        )?;
        batch.publish_diskann_population(&key, &template.encode(), origins)
    }

    pub(in crate::key_value) fn retire_population(
        &self,
        generation: DiskANNGeneration,
        batch: &mut dyn KeyValueBatch,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        let key = Layout::header_key(&self.vectors, generation, control)
            .map_err(VersionError::into_storage_error)?;
        batch.retire_diskann_population(&key)
    }
}

impl KeyValueDiskANNCanonical {
    /// Initialize an older selected generation's derived populations under its original catalog/head guards, without rebuilding the graph or evaluating any row expression.
    pub(in crate::key_value) fn initialize_populations(
        &self,
        index: &crate::RelationIdentity,
        resolver: &dyn crate::diskann_index::catalog::DiskANNIndexResolver,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        let source = self.retain_for_index(index, control)?;
        let selected = source
            .selected_source(resolver, control)?
            .ok_or_else(|| invalid("population initialization has no selected generation"))?;
        let generation = selected.generation();
        if source.read_population(generation, control)?.is_some() {
            return Ok(());
        }
        let scope = source.index_scope(resolver, control)?;
        self.index.store.with_mutation(&mut |read, batch| {
            source.require_current_index(read, batch, control)?;
            publication::require_selected_generation(&scope, read, batch, generation, control)?;
            source.publish_population(&selected, batch, control)
        })
    }
}
