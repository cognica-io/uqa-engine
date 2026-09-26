//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, RetainedDiskANNCanonical};
use crate::diskann_index::{catalog::DiskANNIndexResolver, DiskANNCanonicalRead};
use crate::diskann_index::{
    format::{DiskANNCanonicalOrigin, DiskANNChangeIdentity},
    pages::DiskANNReadLimits,
    DiskANNQuery, DiskANNQueryRead, RetainedDiskANNIndex,
};
use crate::key_value::KeyValueDiskANNSource;
use crate::{read_control::StorageReadControl, StorageBackendResult};
use std::sync::Arc;

impl RetainedDiskANNCanonical {
    /// Consume this actual canonical/catalog view into an owned read-only `VectorIndex`, preserving its physical generation and original controls across nested snapshots and source-session closure.
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

    /// Prepare reusable document search from this actual selected generation and canonical view. Missing publication remains None; malformed selection or unavailable resources fail without substituting an exact index.
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

    /// Keep the selected immutable generation with this exact canonical/catalog view. A retained private publication survives savepoint undo and session closure; an absent head remains absent. No vectors, graph pages or PQ batches are loaded by capture.
    pub fn selected_source(
        &self,
        resolver: &dyn DiskANNIndexResolver,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<Arc<KeyValueDiskANNSource>>> {
        self.check_control(control)?;
        let scope = self.index_scope(resolver, control)?;
        let parameters = self
            .index_parameters()
            .ok_or_else(|| invalid("missing index parameters"))?;
        let source = KeyValueDiskANNSource::select(
            &scope,
            self.dimensions,
            parameters,
            &*self.read,
            &self.control,
            control,
        )?;
        self.check_control(control)?;
        Ok(source)
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
