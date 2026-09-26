//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Session-bound mutations and fresh query snapshots use actual catalog incarnations.

use super::{invalid, KeyValueDiskANNCanonical, RetainedDiskANNCanonical};
use crate::diskann_index::{
    catalog::{DiskANNIndexResolver, DiskANNIndexScope},
    format::DiskANNVectorVersion,
    pages::DiskANNReadLimits,
    RetainedDiskANNIndex,
};
use crate::key_value::{catalog::diskann::Binding, publication::selected_generation};
use crate::{
    read_control::StorageReadControl, vector_index::DiskANNIndexParams, RelationIdentity,
    StorageBackendResult,
};
use std::sync::Arc;
use uqa_core::{memory::MemoryReservation, DocId};

/// A session-bound handle to an already published index. Replacements join the session's original transaction; snapshots own fixed canonical and physical views independently of subsequent writes, undo and session closure. Structural creation, rebuilding and removal belong to the catalog lifecycle owner.
pub struct KeyValueDiskANNHandle {
    canonical: KeyValueDiskANNCanonical,
    index: RelationIdentity,
    resolver: Arc<dyn DiskANNIndexResolver + Send + Sync>,
    scope: DiskANNIndexScope,
    parameters: DiskANNIndexParams,
    limits: DiskANNReadLimits,
    control: StorageReadControl,
    _memory: MemoryReservation,
}

impl KeyValueDiskANNCanonical {
    /// Open a live handle through the actual catalog and published head. Missing or invalid publication is an error; this never creates or adopts an index implicitly.
    pub fn bind(
        self,
        index: RelationIdentity,
        resolver: Arc<dyn DiskANNIndexResolver + Send + Sync>,
        limits: DiskANNReadLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<KeyValueDiskANNHandle> {
        control.check()?;
        let bytes = [
            size_of::<KeyValueDiskANNHandle>(),
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
            .ok_or_else(|| invalid("live index has no bound parameters"))?;
        source
            .selected_source(&*resolver, control)?
            .ok_or_else(|| invalid("live index has no published generation"))?;
        control.check()?;
        Ok(KeyValueDiskANNHandle {
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

impl KeyValueDiskANNHandle {
    /// Atomically replace a complete tensor; an empty slice deletes its vector membership while preserving the replacement origin. Catalog checks and commit requirements are evaluated with the canonical write, not in a preceding read transaction.
    pub fn replace(
        &self,
        document: DocId,
        vectors: &[Vec<f32>],
    ) -> StorageBackendResult<DiskANNVectorVersion> {
        self.control.check()?;
        self.canonical
            .replace_guarded(document, vectors, &self.control, &mut |read, batch| {
                let canonical = &self.canonical.index;
                let binding = Binding::capture(
                    read,
                    &canonical.table,
                    &canonical.field,
                    canonical.dimensions,
                    &self.index,
                    &self.control,
                )?;
                let current = binding.scope(read, &*self.resolver, &self.control, &self.control)?;
                self.validate(&current, binding.parameters)?;
                if selected_generation(&current, read, &self.control)?.is_none() {
                    return Err(invalid("live index has no published generation"));
                }
                binding.require_current(read, batch, &self.control)
            })
    }

    /// Prepare one current query view. The returned `VectorIndex` can be reused and nested without rereading resident PQ data, and keeps its captured generation after this handle advances or closes.
    pub fn snapshot(&self) -> StorageBackendResult<RetainedDiskANNIndex<RetainedDiskANNCanonical>> {
        self.control.check()?;
        let source = self
            .canonical
            .retain_for_index(&self.index, &self.control)?;
        let current = source.index_scope(&*self.resolver, &self.control)?;
        self.validate(
            &current,
            source
                .index_parameters()
                .ok_or_else(|| invalid("live index has no bound parameters"))?,
        )?;
        source
            .into_vector_index(&*self.resolver, self.limits, &self.control)?
            .ok_or_else(|| invalid("live index has no published generation"))
    }

    fn validate(
        &self,
        current: &DiskANNIndexScope,
        parameters: DiskANNIndexParams,
    ) -> StorageBackendResult<()> {
        self.scope.require_same_index(current, &self.control)?;
        if self.parameters != parameters {
            return Err(invalid("live index parameters changed"));
        }
        self.control.check()
    }
}
