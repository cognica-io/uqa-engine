//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    invalid, DiskANNIndexParams, DiskANNReadLimits, DocId, KeyValueDiskANNHandle,
    RetainedDiskANNCanonical, RetainedDiskANNIndex, StorageBackendResult, StorageReadControl,
};
use crate::diskann_index::{
    build::DiskANNTemporaryBudget, DiskANNIndexOptions, DiskANNPersistentOwner, DiskANNQueryRead,
};

impl DiskANNPersistentOwner for KeyValueDiskANNHandle {
    type Snapshot = RetainedDiskANNIndex<RetainedDiskANNCanonical>;
    fn dimensions(&self) -> u32 {
        self.canonical.index.dimensions
    }
    fn parameters(&self) -> DiskANNIndexParams {
        self.parameters
    }
    fn read_limits(&self) -> DiskANNReadLimits {
        self.limits
    }
    fn control(&self) -> &StorageReadControl {
        &self.control
    }
    fn snapshot(&self) -> StorageBackendResult<Self::Snapshot> {
        Self::snapshot(self)
    }
    fn count(&self) -> StorageBackendResult<usize> {
        self.metadata()?.vector_count(&self.control)
    }
    fn contains_document(&self, document: DocId) -> StorageBackendResult<bool> {
        self.metadata()?.contains_vectors(document, &self.control)
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
            .retain_for_index(&self.index, &self.control)?;
        self.validate(
            &source.index_scope(&*self.resolver, &self.control)?,
            options.parameters,
        )?;
        if clear {
            self.canonical.clear_index(
                &source,
                &self.index,
                &*self.resolver,
                options,
                temporary,
                &self.control,
            )
        } else {
            self.canonical.rebuild_source(
                source,
                &*self.resolver,
                options,
                temporary,
                &self.control,
            )
        }
    }
}

impl KeyValueDiskANNHandle {
    fn metadata(&self) -> StorageBackendResult<RetainedDiskANNCanonical> {
        let source = self.retain_current()?;
        source
            .selected_source(&*self.resolver, &self.control)?
            .ok_or_else(|| invalid("live index has no published generation"))?;
        Ok(source)
    }
}
