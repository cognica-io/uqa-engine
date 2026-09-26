//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Structural mutations join the caller's catalog transaction and preserve its outer undo scope.

use super::{invalid, journal, key, KeyValueDiskANNCanonical, Record, RetainedDiskANNCanonical};
use crate::{
    diskann_index::{
        build::DiskANNTemporaryBudget,
        catalog::DiskANNIndexResolver,
        format::{DiskANNChangeIdentity, DiskANNVectorVersion},
        DiskANNIndexOptions,
    },
    key_value::{catalog::diskann::Binding, codec, KeyValueDiskANNStore},
    read_control::StorageReadControl,
    RelationIdentity, StorageBackendError, StorageBackendResult, StorageSavepointId,
};

impl KeyValueDiskANNCanonical {
    /// Adopt the complete existing raw field and publish its first generation. The actual private catalog definition must already exist, and the caller must retain the enclosing definition transaction.
    pub fn create_index(
        &self,
        index: &RelationIdentity,
        resolver: &dyn DiskANNIndexResolver,
        options: DiskANNIndexOptions,
        temporary: &DiskANNTemporaryBudget,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        self.structural_change(|| {
            let source = self.retain_for_index(index, control)?;
            if source.selected_source(resolver, control)?.is_some() {
                return Err(invalid("index already has a published generation"));
            }
            self.adopt(index, control)?;
            self.rebuild_index(index, resolver, options, temporary, control)
        })
    }

    /// Rebuild from one actual retained source. Publication keeps the original head expectation and never completes the caller's transaction.
    pub fn rebuild_index(
        &self,
        index: &RelationIdentity,
        resolver: &dyn DiskANNIndexResolver,
        options: DiskANNIndexOptions,
        temporary: &DiskANNTemporaryBudget,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        let source = self.retain_for_index(index, control)?;
        self.rebuild_source(source, resolver, options, temporary, control)
    }

    pub(super) fn rebuild_source(
        &self,
        source: RetainedDiskANNCanonical,
        resolver: &dyn DiskANNIndexResolver,
        options: DiskANNIndexOptions,
        temporary: &DiskANNTemporaryBudget,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        self.require_transaction()?;
        if source.index_parameters() != Some(options.parameters) {
            return Err(invalid("construction parameters differ from the catalog"));
        }
        let scope = source.index_scope(resolver, control)?;
        let repository = KeyValueDiskANNStore::connect(&self.index.store, control)?;
        repository.initialize(control)?;
        let mut stage = repository.allocate_bound_stage(&scope, control)?;
        let coverage = stage.build(source, options, temporary, control)?;
        let sealed = repository.open_source(stage.generation(), control)?;
        let _prepared = crate::diskann_index::DiskANNQuery::open(
            coverage.source(),
            sealed.clone(),
            options.parameters,
            options.read,
            control,
        )?;
        self.index.store.with_mutation(&mut |read, batch| {
            RetainedDiskANNCanonical::publish_generation(
                &coverage, resolver, &sealed, read, batch, control,
            )
        })
    }

    pub(super) fn clear_index(
        &self,
        source: &RetainedDiskANNCanonical,
        index: &RelationIdentity,
        resolver: &dyn DiskANNIndexResolver,
        options: DiskANNIndexOptions,
        temporary: &DiskANNTemporaryBudget,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        self.structural_change(|| {
            self.index.store.with_mutation(&mut |read, batch| {
                source.require_current_index(read, batch, control)?;
                self.index.stage_clear(batch)?;
                control.check()
            })?;
            self.rebuild_index(index, resolver, options, temporary, control)
        })
    }

    fn adopt(
        &self,
        index: &RelationIdentity,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        control.check()?;
        let _workspace = control.memory().reserve(self.workspace_bytes(true)?)?;
        self.index
            .store
            .with_versioned_mutation(&mut |origin, read, batch| {
                let binding = Binding::capture(
                    read,
                    &self.index.table,
                    &self.index.field,
                    self.index.dimensions,
                    index,
                    control,
                )?;
                binding.require_current(read, batch, control)?;
                let version = DiskANNVectorVersion::new(origin.transaction(), origin.revision())?;
                // Both commit orders are guarded, including insertions absent from this view. Adoption stamps origins without rewriting or copying the canonical vector corpus.
                self.index.coordinate_field(batch, true)?;
                batch.delete_prefix(&super::prefix(&self.index.table, &self.index.field)?)?;
                batch.delete_prefix(&journal::prefix(&self.index.table, &self.index.field)?)?;
                let mut decoder = super::super::read::CanonicalDecoder::new(&self.index);
                let mut current = None;
                let mut count = 0_u64;
                let stamp = |document, count, batch: &mut dyn crate::KeyValueBatch| {
                    let record = Record::new(version, self.index.dimensions, count)?;
                    batch.put(
                        &key(&self.index.table, &self.index.field, document)?,
                        &record.encode(),
                    )?;
                    batch.put(
                        &journal::key(
                            &self.index.table,
                            &self.index.field,
                            DiskANNChangeIdentity::new(document, version),
                        )?,
                        &record.encode(),
                    )
                };
                read.visit_prefix(
                    &codec::vector_field_prefix(&self.index.table, &self.index.field)?,
                    &mut |key, value| {
                        control.check()?;
                        control
                            .check_value_size(value.len(), self.index.dimensions as usize * 4)?;
                        let (document, _, vector) = decoder.decode(key, value)?;
                        crate::vector_index::validate_vector_values_controlled(
                            self.index.dimensions,
                            &vector,
                            Some(control),
                        )?;
                        if current != Some(document) {
                            if let Some(previous) = current {
                                stamp(previous, count, batch)?;
                            }
                            current = Some(document);
                            count = 0;
                        }
                        count = count
                            .checked_add(1)
                            .ok_or_else(|| invalid("canonical count overflow"))?;
                        Ok(())
                    },
                )?;
                if let Some(document) = current {
                    stamp(document, count, batch)?;
                }
                control.check()
            })
    }

    fn structural_change(
        &self,
        operation: impl FnOnce() -> StorageBackendResult<()>,
    ) -> StorageBackendResult<()> {
        self.require_transaction()?;
        let store = &self.index.store;
        let savepoint = StorageSavepointId::allocate().backend_name();
        store.savepoint(&savepoint)?;
        match operation() {
            Ok(()) => store.release_savepoint(&savepoint),
            Err(error) => {
                store.rollback_to_savepoint(&savepoint).and_then(|()| store.release_savepoint(&savepoint))
                    .map_err(|rollback| StorageBackendError::Other(format!("DiskANN structural rollback failed: {rollback}; original error: {error}")))?;
                Err(error)
            }
        }
    }

    fn require_transaction(&self) -> StorageBackendResult<()> {
        if !self.index.store.in_transaction() {
            return Err(invalid(
                "structural changes require the caller's active catalog transaction",
            ));
        }
        Ok(())
    }
}
