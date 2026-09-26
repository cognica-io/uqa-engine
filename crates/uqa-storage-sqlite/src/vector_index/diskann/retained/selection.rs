//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, RetainedSQLiteDiskANNCanonical};
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

impl RetainedSQLiteDiskANNCanonical {
    /// Consume the retained native view into the common read-only `VectorIndex` owner without copying canonical vectors or reconstructing the graph.
    pub fn into_vector_index(
        self,
        resolver: &dyn DiskANNIndexResolver,
        limits: DiskANNReadLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<RetainedDiskANNIndex<Self>>> {
        self.selected_source(resolver, control)?
            .map(|source| {
                let parameters = self
                    .index_parameters()
                    .ok_or_else(|| invalid("missing index parameters"))?;
                RetainedDiskANNIndex::open(self, source, parameters, limits, control)
            })
            .transpose()
    }

    /// Prepare the common Storage document search on this native canonical/catalog view and its selected physical source.
    pub fn query(
        &self,
        resolver: &dyn DiskANNIndexResolver,
        limits: DiskANNReadLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNQuery<'_>>> {
        self.selected_source(resolver, control)?
            .map(|source| {
                DiskANNQuery::open(
                    self,
                    source,
                    self.index_parameters()
                        .ok_or_else(|| invalid("missing index parameters"))?,
                    limits,
                    control,
                )
            })
            .transpose()
    }

    /// Select physical graph data on this native canonical/catalog view, retaining private publication resources without advancing the SQL snapshot or loading page/PQ bodies.
    pub fn selected_source(
        &self,
        resolver: &dyn DiskANNIndexResolver,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<Arc<KeyValueDiskANNSource>>> {
        self.check(control)?;
        let scope = self.index_scope(resolver, control)?;
        let parameters = self
            .index_parameters()
            .ok_or_else(|| invalid("missing index parameters"))?;
        let records = self.snapshot.record_read();
        let physical = crate::diskann::map_read(&records, self.snapshot.database)?;
        let source = KeyValueDiskANNSource::select(
            &scope,
            self.dimensions,
            parameters,
            &physical,
            &self.control,
            control,
        )?;
        self.check(control)?;
        Ok(source)
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
