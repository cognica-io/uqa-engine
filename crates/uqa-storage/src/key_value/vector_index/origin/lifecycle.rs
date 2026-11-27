//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Structural mutations join the caller's catalog transaction and preserve its outer undo scope.

use super::{journal, key, KeyValueDiskANNCanonical, Record, RetainedDiskANNCanonical};
use crate::{
    diskann_index::{
        build::{DiskANNCanonicalCoverage, DiskANNTemporaryBudget},
        catalog::DiskANNIndexResolver,
        format::{DiskANNChangeIdentity, DiskANNVectorVersion},
        DiskANNAdoptedOrigins, DiskANNIndexOptions, DiskANNLifecycle,
    },
    key_value::{catalog::diskann::Binding, codec, KeyValueDiskANNSource, KeyValueDiskANNStore},
    read_control::StorageReadControl,
    RelationIdentity, StorageBackendResult,
};

impl KeyValueDiskANNCanonical {
    /// Retire the selected generation before removing its SQL catalog definition. Raw vectors remain available to the replacement exact index, while stale writers and the old live handle can no longer mutate this field through its retired selection.
    pub fn retire_index(
        &self,
        index: &RelationIdentity,
        resolver: &dyn DiskANNIndexResolver,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        DiskANNLifecycle::retire_index(self, index, resolver, control)
    }
    /// Adopt the complete existing raw field and publish its first generation. The actual private catalog definition must already exist, and the caller must retain the enclosing definition transaction.
    pub fn create_index(
        &self,
        index: &RelationIdentity,
        resolver: &dyn DiskANNIndexResolver,
        options: DiskANNIndexOptions,
        temporary: &DiskANNTemporaryBudget,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        DiskANNLifecycle::create_index(self, index, resolver, options, temporary, control)
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
        DiskANNLifecycle::rebuild_index(self, index, resolver, options, temporary, control)
    }
}

impl DiskANNLifecycle for KeyValueDiskANNCanonical {
    type Read = RetainedDiskANNCanonical;
    type SavepointError = crate::StorageBackendError;
    fn in_transaction(&self) -> bool {
        self.index.store.in_transaction()
    }
    fn savepoint(&self, name: &str) -> Result<(), Self::SavepointError> {
        self.index.store.savepoint(name)
    }
    fn release_savepoint(&self, name: &str) -> Result<(), Self::SavepointError> {
        self.index.store.release_savepoint(name)
    }
    fn rollback_to_savepoint(&self, name: &str) -> Result<(), Self::SavepointError> {
        self.index.store.rollback_to_savepoint(name)
    }
    fn retain_for_index(
        &self,
        index: &RelationIdentity,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self::Read> {
        Self::retain_for_index(self, index, control)
    }
    fn generation_store(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<KeyValueDiskANNStore> {
        KeyValueDiskANNStore::connect(&self.index.store, control)
    }
    fn publish(
        &self,
        coverage: &DiskANNCanonicalCoverage<Self::Read>,
        resolver: &dyn DiskANNIndexResolver,
        sealed: &KeyValueDiskANNSource,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        self.index.store.with_mutation(&mut |read, batch| {
            RetainedDiskANNCanonical::publish_generation(
                coverage, resolver, sealed, read, batch, control,
            )
        })
    }
    fn retire_generation(
        &self,
        source: &Self::Read,
        resolver: &dyn DiskANNIndexResolver,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        self.index.store.with_mutation(&mut |read, batch| {
            source.retire_generation(resolver, read, batch, control)?;
            self.index.coordinate_field(batch, true)?;
            control.check()
        })
    }
    fn clear_vectors(
        &self,
        source: &Self::Read,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        self.index.store.with_mutation(&mut |read, batch| {
            source.require_current_index(read, batch, control)?;
            self.index.stage_clear(batch)?;
            control.check()
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
                batch.delete_prefix_allow_absent(&journal::prefix(
                    &self.index.table,
                    &self.index.field,
                )?)?;
                let mut decoder = super::super::read::CanonicalDecoder::new(&self.index);
                let mut origins = DiskANNAdoptedOrigins::new(
                    version,
                    self.index.dimensions,
                    |document, record: Record| {
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
                    },
                );
                read.visit_prefix(
                    &codec::vector_field_prefix(&self.index.table, &self.index.field)?,
                    &mut |key, value| {
                        control.check()?;
                        control
                            .check_value_size(value.len(), self.index.dimensions as usize * 4)?;
                        let (document, ordinal, vector) = decoder.decode(key, value)?;
                        crate::vector_index::validate_vector_values_controlled(
                            self.index.dimensions,
                            &vector,
                            Some(control),
                        )?;
                        origins.push(document, ordinal)
                    },
                )?;
                origins.finish()?;
                control.check()
            })
    }
}
