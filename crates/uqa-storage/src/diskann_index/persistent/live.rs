//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistent reads and guarded writes share the binding established on the publishing catalog view.

use super::{invalid, DiskANNBoundRead, DiskANNPersistentBackend, DiskANNPersistentOwner};
use crate::{
    diskann_index::{
        build::DiskANNTemporaryBudget,
        catalog::{DiskANNIndexResolver, DiskANNIndexScope},
        format::DiskANNVectorVersion,
        pages::DiskANNReadLimits,
        DiskANNIndexOptions, DiskANNQueryRead, RetainedDiskANNIndex,
    },
    key_value::{publication::selected_generation, KeyValueRead},
    read_control::StorageReadControl,
    vector_index::DiskANNIndexParams,
    RelationIdentity, StorageBackendResult,
};
use std::{marker::PhantomData, sync::Arc};
use uqa_core::{
    memory::{MemoryError, MemoryReservation},
    DocId,
};

/// The catalog and publication evidence used inside a canonical write.
pub struct DiskANNLiveBinding<B> {
    pub index: RelationIdentity,
    pub resolver: Arc<dyn DiskANNIndexResolver + Send + Sync>,
    scope: DiskANNIndexScope,
    parameters: DiskANNIndexParams,
    limits: DiskANNReadLimits,
    pub control: StorageReadControl,
    provider: PhantomData<fn() -> B>,
}

impl<B: DiskANNPersistentBackend> DiskANNLiveBinding<B> {
    pub fn validate(
        &self,
        current: &DiskANNIndexScope,
        parameters: DiskANNIndexParams,
    ) -> StorageBackendResult<()> {
        self.scope.require_same_index(current, &self.control)?;
        if self.parameters != parameters {
            return Err(invalid(B::CHANGED_PARAMETERS));
        }
        self.control.check()
    }

    pub fn require_publication(
        &self,
        scope: &DiskANNIndexScope,
        read: &dyn KeyValueRead,
    ) -> StorageBackendResult<()> {
        if selected_generation(scope, read, &self.control)?.is_none() {
            return Err(invalid(B::MISSING_PUBLICATION));
        }
        Ok(())
    }
}

/// A session-bound handle to a published index. Each returned snapshot retains its generation through later writes, undo and session closure.
pub struct PersistentDiskANNHandle<B> {
    canonical: B,
    binding: DiskANNLiveBinding<B>,
    _memory: MemoryReservation,
}

impl<B: DiskANNPersistentBackend> PersistentDiskANNHandle<B> {
    pub fn bind(
        canonical: B,
        index: RelationIdentity,
        resolver: Arc<dyn DiskANNIndexResolver + Send + Sync>,
        limits: DiskANNReadLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let [table, field] = canonical.name_payload_bytes();
        let bytes = [
            size_of::<Self>(),
            table,
            field,
            index.schema.capacity(),
            index.name.capacity(),
        ]
        .into_iter()
        .try_fold(0_usize, usize::checked_add)
        .ok_or(MemoryError::SizeOverflow)?;
        let memory = control.memory().reserve(bytes)?;
        canonical.initialize_populations(&index, &*resolver, control)?;
        let source = canonical.retain_for_index(&index, control)?;
        let scope = source.index_scope(&*resolver, control)?;
        let parameters = source
            .index_parameters()
            .ok_or_else(|| invalid(B::MISSING_PARAMETERS))?;
        source
            .selected_source(&*resolver, control)?
            .ok_or_else(|| invalid(B::MISSING_PUBLICATION))?;
        control.check()?;
        Ok(Self {
            canonical,
            binding: DiskANNLiveBinding {
                index,
                resolver,
                scope,
                parameters,
                limits,
                control: control.clone(),
                provider: PhantomData,
            },
            _memory: memory,
        })
    }

    /// Validate the binding and replace every tensor ordinal in one transaction.
    pub fn replace(
        &self,
        document: DocId,
        vectors: &[Vec<f32>],
    ) -> StorageBackendResult<DiskANNVectorVersion> {
        self.binding.control.check()?;
        self.canonical
            .replace_bound(&self.binding, document, vectors)
    }

    /// Prepare one current query view. The returned `VectorIndex` can be reused and nested without rereading resident PQ data, and keeps its captured generation after this handle advances or closes.
    pub fn snapshot(&self) -> StorageBackendResult<RetainedDiskANNIndex<B::Read>> {
        self.retain_current()?
            .into_vector_index(
                &*self.binding.resolver,
                self.binding.limits,
                &self.binding.control,
            )?
            .ok_or_else(|| invalid(B::MISSING_PUBLICATION))
    }

    fn retain_current(&self) -> StorageBackendResult<B::Read> {
        self.binding.control.check()?;
        let source = self
            .canonical
            .retain_for_index(&self.binding.index, &self.binding.control)?;
        let current = source.index_scope(&*self.binding.resolver, &self.binding.control)?;
        self.binding.validate(
            &current,
            source
                .index_parameters()
                .ok_or_else(|| invalid(B::MISSING_PARAMETERS))?,
        )?;
        Ok(source)
    }
}

impl<B: DiskANNPersistentBackend> DiskANNPersistentOwner for PersistentDiskANNHandle<B> {
    type Snapshot = RetainedDiskANNIndex<B::Read>;
    fn dimensions(&self) -> u32 {
        self.canonical.dimensions()
    }
    fn parameters(&self) -> DiskANNIndexParams {
        self.binding.parameters
    }
    fn read_limits(&self) -> DiskANNReadLimits {
        self.binding.limits
    }
    fn control(&self) -> &StorageReadControl {
        &self.binding.control
    }
    fn query_metadata(&self) -> StorageBackendResult<crate::diskann_index::DiskANNQueryMetadata> {
        let canonical = self.retain_current()?;
        let source = canonical
            .selected_source(&*self.binding.resolver, &self.binding.control)?
            .ok_or_else(|| invalid(B::MISSING_PUBLICATION))?;
        crate::diskann_index::DiskANNQueryMetadata::capture(
            &canonical,
            &*source,
            self.binding.parameters,
            self.binding.limits,
            &self.binding.control,
        )
    }
    fn snapshot(&self) -> StorageBackendResult<Self::Snapshot> {
        Self::snapshot(self)
    }
    fn count(&self) -> StorageBackendResult<usize> {
        self.metadata()?.vector_count(&self.binding.control)
    }
    fn contains_document(&self, document: DocId) -> StorageBackendResult<bool> {
        self.metadata()?
            .contains_vectors(document, &self.binding.control)
    }
    fn replace(&self, document: DocId, vectors: &[Vec<f32>]) -> StorageBackendResult<()> {
        Self::replace(self, document, vectors).map(|_| ())
    }
    fn rebuild(
        &self,
        options: DiskANNIndexOptions,
        temporary: &DiskANNTemporaryBudget,
        clear: bool,
    ) -> StorageBackendResult<()> {
        let source = self
            .canonical
            .retain_for_index(&self.binding.index, &self.binding.control)?;
        self.binding.validate(
            &source.index_scope(&*self.binding.resolver, &self.binding.control)?,
            options.parameters,
        )?;
        if clear {
            super::lifecycle::structural_change(&self.canonical, || {
                self.canonical
                    .clear_vectors(&source, &self.binding.control)?;
                self.canonical.rebuild_index(
                    &self.binding.index,
                    &*self.binding.resolver,
                    options,
                    temporary,
                    &self.binding.control,
                )
            })
        } else {
            self.canonical.rebuild_source(
                source,
                &*self.binding.resolver,
                options,
                temporary,
                &self.binding.control,
            )
        }
    }
}

impl<B: DiskANNPersistentBackend> PersistentDiskANNHandle<B> {
    fn metadata(&self) -> StorageBackendResult<B::Read> {
        let source = self.retain_current()?;
        source
            .selected_source(&*self.binding.resolver, &self.binding.control)?
            .ok_or_else(|| invalid(B::MISSING_PUBLICATION))?;
        Ok(source)
    }
}
