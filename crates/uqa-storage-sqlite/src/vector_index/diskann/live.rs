//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native live writes guard catalog records on the evaluated canonical boundary.

mod runtime;

use super::{invalid, RetainedSQLiteDiskANNCanonical, SQLiteDiskANNCanonical};
use std::sync::Arc;
use uqa_core::{memory::MemoryReservation, DocId};
use uqa_storage::{
    diskann_index::{
        catalog::{DiskANNIndexResolver, DiskANNIndexScope},
        format::DiskANNVectorVersion,
        pages::DiskANNReadLimits,
        RetainedDiskANNIndex,
    },
    key_value::publication::selected_generation,
    read_control::StorageReadControl,
    vector_index::DiskANNIndexParams,
    RelationIdentity, StorageBackendResult,
};

/// A native session-bound handle to an already published index. Complete tensor mutations join the caller's transaction, while query snapshots retain their original catalog, canonical and physical views. Catalog lifecycle owners perform structural creation, rebuilding and removal.
pub struct SQLiteDiskANNHandle {
    canonical: SQLiteDiskANNCanonical,
    index: RelationIdentity,
    resolver: Arc<dyn DiskANNIndexResolver + Send + Sync>,
    scope: DiskANNIndexScope,
    parameters: DiskANNIndexParams,
    limits: DiskANNReadLimits,
    control: StorageReadControl,
    _memory: MemoryReservation,
}

impl SQLiteDiskANNCanonical {
    /// Bind through the actual native catalog and published head. No generation is implicitly created, adopted or replaced.
    pub fn bind(
        self,
        index: RelationIdentity,
        resolver: Arc<dyn DiskANNIndexResolver + Send + Sync>,
        limits: DiskANNReadLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<SQLiteDiskANNHandle> {
        control.check()?;
        let bytes = [
            size_of::<SQLiteDiskANNHandle>(),
            self.index.table.capacity(),
            self.index.field.capacity(),
            index.schema.capacity(),
            index.name.capacity(),
        ]
        .into_iter()
        .try_fold(0_usize, usize::checked_add)
        .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
        let memory = control.memory().reserve(bytes)?;
        let source = self.retain_for_index(&index, control)?;
        let scope = source.index_scope(&*resolver, control)?;
        let parameters = source
            .index_parameters()
            .ok_or_else(|| invalid("live native index has no bound parameters"))?;
        source
            .selected_source(&*resolver, control)?
            .ok_or_else(|| invalid("live native index has no published generation"))?;
        control.check()?;
        Ok(SQLiteDiskANNHandle {
            canonical: self,
            index,
            resolver,
            scope,
            parameters,
            limits,
            control: control.clone(),
            _memory: memory,
        })
    }
}

impl SQLiteDiskANNHandle {
    /// Replace all ordinals and their origin/change record atomically. Empty tensors remove vector membership. The actual native catalog records become commit requirements in this same evaluated mutation.
    pub fn replace(
        &self,
        document: DocId,
        vectors: &[Vec<f32>],
    ) -> StorageBackendResult<DiskANNVectorVersion> {
        self.control.check()?;
        self.canonical
            .replace_guarded(document, vectors, &self.control, |snapshot, batch| {
                let canonical = &self.canonical.index;
                let binding = crate::catalog::DiskANNCatalogBinding::capture(
                    snapshot,
                    &canonical.table,
                    &canonical.field,
                    canonical.dimensions,
                    &self.index,
                    &self.control,
                )?;
                let current =
                    binding.scope(snapshot, &*self.resolver, &self.control, &self.control)?;
                self.validate(&current, binding.parameters)?;
                let read = snapshot.record_read();
                let physical = crate::diskann::map_read(&read, snapshot.database)?;
                if selected_generation(&current, &physical, &self.control)?.is_none() {
                    return Err(invalid("live native index has no published generation"));
                }
                binding.require_current(&read, batch, &self.control)
            })
    }

    /// Prepare a fresh query snapshot; repeated and nested reads of the returned index share resident data and keep the original generation through subsequent writes, undo and session closure.
    pub fn snapshot(
        &self,
    ) -> StorageBackendResult<RetainedDiskANNIndex<RetainedSQLiteDiskANNCanonical>> {
        self.retain_current()?
            .into_vector_index(&*self.resolver, self.limits, &self.control)?
            .ok_or_else(|| invalid("live native index has no published generation"))
    }

    fn retain_current(&self) -> StorageBackendResult<RetainedSQLiteDiskANNCanonical> {
        self.control.check()?;
        let source = self
            .canonical
            .retain_for_index(&self.index, &self.control)?;
        let current = source.index_scope(&*self.resolver, &self.control)?;
        self.validate(
            &current,
            source
                .index_parameters()
                .ok_or_else(|| invalid("live native index has no bound parameters"))?,
        )?;
        Ok(source)
    }

    fn validate(
        &self,
        current: &DiskANNIndexScope,
        parameters: DiskANNIndexParams,
    ) -> StorageBackendResult<()> {
        self.scope.require_same_index(current, &self.control)?;
        if self.parameters != parameters {
            return Err(invalid("live native index parameters changed"));
        }
        self.control.check()
    }
}
