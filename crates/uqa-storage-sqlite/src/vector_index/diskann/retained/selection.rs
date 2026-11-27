//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::RetainedSQLiteDiskANNCanonical;
use std::sync::Arc;
use uqa_storage::diskann_index::{
    format::{DiskANNCanonicalOrigin, DiskANNChangeIdentity},
    pages::DiskANNReadLimits,
    DiskANNQuery, DiskANNQueryRead, RetainedDiskANNIndex,
};
use uqa_storage::{
    diskann_index::catalog::DiskANNIndexResolver, key_value::KeyValueDiskANNSource,
    read_control::StorageReadControl, StorageBackendResult,
};

use uqa_storage::{
    diskann_index::{catalog::DiskANNIndexScope, DiskANNBoundRead, DiskANNMutationView},
    key_value::KeyValueRead,
    vector_index::DiskANNIndexParams,
};

impl DiskANNBoundRead for RetainedSQLiteDiskANNCanonical {
    type DocumentKey = i64;
    const MISSING_ORDINAL: &'static str = "missing native canonical ordinal";
    const WRONG_WIDTH: &'static str = "native canonical vector width mismatch";
    fn document_key(
        &self,
        document: uqa_core::DocId,
        _: &StorageReadControl,
    ) -> StorageBackendResult<Self::DocumentKey> {
        super::encode_doc_id(document).map_err(Into::into)
    }
    fn read_ordinal_bytes(
        &self,
        key: &Self::DocumentKey,
        ordinal: u64,
        max_bytes: usize,
        control: &StorageReadControl,
        visit: &mut uqa_storage::read_control::ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.read_payload(*key, Some(ordinal), max_bytes, control, visit)
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
        self.snapshot.with_generation_read(visit)
    }
}

impl RetainedSQLiteDiskANNCanonical {
    /// Consume the retained native view into the common read-only `VectorIndex` owner without copying canonical vectors or reconstructing the graph.
    pub fn into_vector_index(
        self,
        resolver: &dyn DiskANNIndexResolver,
        limits: DiskANNReadLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<RetainedDiskANNIndex<Self>>> {
        DiskANNBoundRead::into_vector_index(self, resolver, limits, control)
    }

    /// Prepare the common Storage document search on this native canonical/catalog view and its selected physical source.
    pub fn query(
        &self,
        resolver: &dyn DiskANNIndexResolver,
        limits: DiskANNReadLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNQuery<'_>>> {
        DiskANNBoundRead::query(self, resolver, limits, control)
    }

    /// Select physical graph data on this native canonical/catalog view, retaining private publication resources without advancing the SQL snapshot or loading page/PQ bodies.
    pub fn selected_source(
        &self,
        resolver: &dyn DiskANNIndexResolver,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<Arc<KeyValueDiskANNSource>>> {
        DiskANNBoundRead::selected_source(self, resolver, control)
    }
}

impl DiskANNQueryRead for RetainedSQLiteDiskANNCanonical {
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
