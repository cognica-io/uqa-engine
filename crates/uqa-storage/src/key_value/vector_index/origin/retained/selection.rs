//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::RetainedDiskANNCanonical;
use crate::diskann_index::catalog::DiskANNIndexResolver;
use crate::diskann_index::{
    format::{DiskANNCanonicalOrigin, DiskANNChangeIdentity},
    pages::DiskANNReadLimits,
    DiskANNQuery, DiskANNQueryRead, RetainedDiskANNIndex,
};
use crate::key_value::KeyValueDiskANNSource;
use crate::{read_control::StorageReadControl, StorageBackendResult};
use std::sync::Arc;

use crate::{
    diskann_index::{catalog::DiskANNIndexScope, DiskANNBoundRead},
    key_value::KeyValueRead,
    vector_index::DiskANNIndexParams,
};

impl DiskANNBoundRead for RetainedDiskANNCanonical {
    type DocumentKey = uqa_core::memory::BudgetedVec<u8>;
    const MISSING_ORDINAL: &'static str = "missing canonical ordinal";
    const WRONG_WIDTH: &'static str = "canonical vector width mismatch";
    fn document_key(
        &self,
        document: uqa_core::DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self::DocumentKey> {
        super::append(&self.vectors, &document.to_be_bytes(), control)
    }
    fn read_ordinal_bytes(
        &self,
        key: &Self::DocumentKey,
        ordinal: u64,
        max_bytes: usize,
        control: &StorageReadControl,
        visit: &mut crate::read_control::ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.control.check()?;
        control.check()?;
        let key = super::append(key, &ordinal.to_be_bytes(), control)?;
        self.read
            .visit_value_bounded(&key, max_bytes, control, visit)
    }
    fn index_scope(
        &self,
        resolver: &dyn DiskANNIndexResolver,
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNIndexScope> {
        Self::index_scope(self, resolver, control)
    }
    fn index_parameters(&self) -> Option<DiskANNIndexParams> {
        Self::index_parameters(self)
    }
    fn original_control(&self) -> &StorageReadControl {
        &self.control
    }
    fn with_physical_read<T>(
        &self,
        visit: impl FnOnce(&dyn KeyValueRead) -> StorageBackendResult<T>,
    ) -> StorageBackendResult<T> {
        visit(&*self.read)
    }
}

impl RetainedDiskANNCanonical {
    /// Consume this actual canonical/catalog view into an owned read-only `VectorIndex`, preserving its physical generation and original controls across nested snapshots and source-session closure.
    pub fn into_vector_index(
        self,
        resolver: &dyn DiskANNIndexResolver,
        limits: DiskANNReadLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<RetainedDiskANNIndex<Self>>> {
        DiskANNBoundRead::into_vector_index(self, resolver, limits, control)
    }

    /// Prepare reusable document search from this actual selected generation and canonical view. Missing publication remains None; malformed selection or unavailable resources fail without substituting an exact index.
    pub fn query(
        &self,
        resolver: &dyn DiskANNIndexResolver,
        limits: DiskANNReadLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNQuery<'_>>> {
        DiskANNBoundRead::query(self, resolver, limits, control)
    }

    /// Keep the selected immutable generation with this exact canonical/catalog view. A retained private publication survives savepoint undo and session closure; an absent head remains absent. No vectors, graph pages or PQ batches are loaded by capture.
    pub fn selected_source(
        &self,
        resolver: &dyn DiskANNIndexResolver,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<Arc<KeyValueDiskANNSource>>> {
        DiskANNBoundRead::selected_source(self, resolver, control)
    }
}

impl DiskANNQueryRead for RetainedDiskANNCanonical {
    fn document_origin(
        &self,
        document: uqa_core::DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNCanonicalOrigin>> {
        self.record(document, control)
    }

    fn next_change_after(
        &self,
        after: Option<uqa_core::DocId>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNChangeIdentity>> {
        self.next_change_after(after, control)
    }
}
