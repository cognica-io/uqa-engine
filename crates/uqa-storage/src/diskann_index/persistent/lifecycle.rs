//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog creates, rebuilds, clears and retirements preserve the caller's transaction and its original expected publication head.

use super::{invalid, DiskANNBoundRead};
use crate::{
    diskann_index::{
        build::{DiskANNCanonicalCoverage, DiskANNTemporaryBudget},
        catalog::DiskANNIndexResolver,
        DiskANNIndexOptions, DiskANNQuery,
    },
    key_value::{KeyValueDiskANNSource, KeyValueDiskANNStore},
    read_control::StorageReadControl,
    RelationIdentity, StorageBackendError, StorageBackendResult, StorageSavepointId,
};

/// Structural transactions and generation construction shared by both persistent record layouts.
pub trait DiskANNLifecycle: Send + Sync + Sized + 'static {
    type Read: DiskANNBoundRead;
    fn in_transaction(&self) -> bool;
    fn retain_for_index(
        &self,
        index: &RelationIdentity,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self::Read>;
    type SavepointError: std::fmt::Display + Into<StorageBackendError>;
    fn savepoint(&self, name: &str) -> Result<(), Self::SavepointError>;
    fn release_savepoint(&self, name: &str) -> Result<(), Self::SavepointError>;
    fn rollback_to_savepoint(&self, name: &str) -> Result<(), Self::SavepointError>;
    fn generation_store(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<KeyValueDiskANNStore>;
    fn publish(
        &self,
        coverage: &DiskANNCanonicalCoverage<Self::Read>,
        resolver: &dyn DiskANNIndexResolver,
        sealed: &KeyValueDiskANNSource,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()>;
    fn adopt(
        &self,
        index: &RelationIdentity,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()>;
    fn clear_vectors(
        &self,
        source: &Self::Read,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()>;
    fn retire_generation(
        &self,
        source: &Self::Read,
        resolver: &dyn DiskANNIndexResolver,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()>;

    fn create_index(
        &self,
        index: &RelationIdentity,
        resolver: &dyn DiskANNIndexResolver,
        options: DiskANNIndexOptions,
        temporary: &DiskANNTemporaryBudget,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        structural_change(self, || {
            if self
                .retain_for_index(index, control)?
                .selected_source(resolver, control)?
                .is_some()
            {
                return Err(invalid("index already has a published generation"));
            }
            self.adopt(index, control)?;
            self.rebuild_index(index, resolver, options, temporary, control)
        })
    }

    fn rebuild_index(
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

    fn rebuild_source(
        &self,
        source: Self::Read,
        resolver: &dyn DiskANNIndexResolver,
        options: DiskANNIndexOptions,
        temporary: &DiskANNTemporaryBudget,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        require_transaction(self)?;
        if source.index_parameters() != Some(options.parameters) {
            return Err(invalid("construction parameters differ from the catalog"));
        }
        let scope = source.index_scope(resolver, control)?;
        let repository = self.generation_store(control)?;
        repository.initialize(control)?;
        let mut stage = repository.allocate_bound_stage(&scope, control)?;
        let coverage = stage.build(source, options, temporary, control)?;
        let sealed = repository.open_source(stage.generation(), control)?;
        let _prepared = DiskANNQuery::open(
            coverage.source(),
            sealed.clone(),
            options.parameters,
            options.read,
            control,
        )?;
        self.publish(&coverage, resolver, &sealed, control)
    }

    fn retire_index(
        &self,
        index: &RelationIdentity,
        resolver: &dyn DiskANNIndexResolver,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        structural_change(self, || {
            let source = self.retain_for_index(index, control)?;
            self.retire_generation(&source, resolver, control)
        })
    }
}

fn require_transaction<B: DiskANNLifecycle>(backend: &B) -> StorageBackendResult<()> {
    if !backend.in_transaction() {
        return Err(invalid(
            "structural changes require the caller's active catalog transaction",
        ));
    }
    Ok(())
}

pub(super) fn structural_change<B: DiskANNLifecycle>(
    backend: &B,
    operation: impl FnOnce() -> StorageBackendResult<()>,
) -> StorageBackendResult<()> {
    require_transaction(backend)?;
    let name = StorageSavepointId::allocate().backend_name();
    backend.savepoint(&name).map_err(Into::into)?;
    match operation() {
        Ok(()) => backend.release_savepoint(&name).map_err(Into::into),
        Err(error) => {
            backend
                .rollback_to_savepoint(&name)
                .and_then(|()| backend.release_savepoint(&name))
                .map_err(|rollback| {
                    StorageBackendError::Other(format!(
                        "DiskANN structural rollback failed: {rollback}; original error: {error}"
                    ))
                })?;
            Err(error)
        }
    }
}
