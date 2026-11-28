//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Select the physical generation and canonical records from one retained catalog view.

use crate::diskann_index::{
    catalog::{DiskANNIndexResolver, DiskANNIndexScope},
    changes::DiskANNChangeRead,
    format::{DiskANNCanonicalOrigin, DiskANNChangeIdentity, DiskANNVectorVersion},
    maintenance::{DiskANNStatisticsPage, DiskANNStatisticsRequest},
    pages::DiskANNReadLimits,
    DiskANNCanonicalVectorVisitor, DiskANNQuery, DiskANNQueryRead, RetainedDiskANNIndex,
};
use crate::key_value::{KeyValueDiskANNPruner, KeyValueDiskANNSource, KeyValueRead};
use crate::{
    read_control::{StorageReadControl, ValueReadVisitor},
    vector_index::{decode_vector_coordinates, DiskANNIndexParams},
    StorageBackendResult,
};
use std::sync::Arc;
use uqa_core::{
    memory::{BudgetedVec, MemoryError},
    DocId,
};

/// A canonical source and its catalog binding on the same persistent snapshot. The physical adapter supplies the generation records for this view.
pub trait DiskANNBoundRead: DiskANNQueryRead + Send + Sync + Sized + 'static {
    type DocumentKey;
    const MISSING_ORDINAL: &'static str;
    const WRONG_WIDTH: &'static str;
    fn document_key(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self::DocumentKey>;
    fn read_ordinal_bytes(
        &self,
        key: &Self::DocumentKey,
        ordinal: u64,
        max_bytes: usize,
        control: &StorageReadControl,
        visit: &mut ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()>;
    fn index_scope(
        &self,
        resolver: &dyn DiskANNIndexResolver,
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNIndexScope>;
    fn index_parameters(&self) -> Option<DiskANNIndexParams>;
    fn original_control(&self) -> &StorageReadControl;
    fn with_physical_read<T>(
        &self,
        read: impl FnOnce(&dyn KeyValueRead) -> StorageBackendResult<T>,
    ) -> StorageBackendResult<T>;

    /// Visit every ordinal of the verified replacement with one charged coordinate buffer.
    fn visit_canonical_document(
        &self,
        document: DocId,
        control: &StorageReadControl,
        visit: &mut DiskANNCanonicalVectorVisitor<'_>,
    ) -> StorageBackendResult<Option<DiskANNVectorVersion>> {
        let Some(record) = self.document_origin(document, control)? else {
            return Ok(None);
        };
        if record.count() == 0 {
            return Ok(Some(record.version()));
        }
        let key = self.document_key(document, control)?;
        let dimensions = usize::try_from(self.dimensions())
            .map_err(|_| super::invalid("canonical dimensions exceed platform size"))?;
        let width = dimensions.checked_mul(4).ok_or(MemoryError::SizeOverflow)?;
        let mut vector = BudgetedVec::new(control.memory());
        vector.reserve(dimensions)?;
        for ordinal in 0..record.count() {
            self.read_ordinal_bytes(&key, ordinal, width, control, &mut |value| {
                let value = value.ok_or_else(|| super::invalid(Self::MISSING_ORDINAL))?;
                if value.len() != width {
                    return Err(super::invalid(Self::WRONG_WIDTH));
                }
                decode_vector_coordinates(value, self.dimensions(), &mut vector, control, || {
                    self.check_control(control)
                })?;
                let ordinal = u32::try_from(ordinal)
                    .map_err(|_| super::invalid("canonical ordinal overflow"))?;
                visit(ordinal, record.version(), &vector)
            })?;
        }
        self.check_control(control)?;
        Ok(Some(record.version()))
    }

    /// Skip obsolete journal entries until a change matches the document's complete visible origin.
    fn next_canonical_change(
        &self,
        mut after: Option<DocId>,
        maximum: DocId,
        control: &StorageReadControl,
        next: impl Fn(Option<DocId>) -> StorageBackendResult<Option<DocId>>,
        matches: impl Fn(DiskANNChangeIdentity, DiskANNCanonicalOrigin) -> StorageBackendResult<bool>,
    ) -> StorageBackendResult<Option<DiskANNChangeIdentity>> {
        loop {
            self.check_control(control)?;
            if after.is_some_and(|document| document >= maximum) {
                return Ok(None);
            }
            let Some(document) = next(after)? else {
                self.check_control(control)?;
                return Ok(None);
            };
            after = Some(document);
            if let Some(origin) = self.document_origin(document, control)? {
                let change = DiskANNChangeIdentity::new(document, origin.version());
                if matches(change, origin)? {
                    self.check_control(control)?;
                    return Ok(Some(change));
                }
            }
        }
    }

    fn measure_journal(
        &self,
        pruner: &KeyValueDiskANNPruner,
        read: &dyn DiskANNChangeRead,
        request: DiskANNStatisticsRequest,
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNStatisticsPage> {
        self.check_control(control)?;
        let page = pruner.statistics(read, request, control)?;
        self.check_control(control)?;
        Ok(page)
    }

    fn selected_source(
        &self,
        resolver: &dyn DiskANNIndexResolver,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<Arc<KeyValueDiskANNSource>>> {
        self.check_control(control)?;
        let scope = self.index_scope(resolver, control)?;
        let parameters = self
            .index_parameters()
            .ok_or_else(|| super::invalid("missing index parameters"))?;
        let source = self.with_physical_read(|read| {
            KeyValueDiskANNSource::select(
                &scope,
                self.dimensions(),
                parameters,
                read,
                self.original_control(),
                control,
            )
        })?;
        self.check_control(control)?;
        Ok(source)
    }

    fn into_vector_index(
        self,
        resolver: &dyn DiskANNIndexResolver,
        limits: DiskANNReadLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<RetainedDiskANNIndex<Self>>> {
        self.selected_source(resolver, control)?
            .map(|source| {
                let parameters = self
                    .index_parameters()
                    .ok_or_else(|| super::invalid("missing index parameters"))?;
                RetainedDiskANNIndex::open(self, source, parameters, limits, control)
            })
            .transpose()
    }

    fn query(
        &self,
        resolver: &dyn DiskANNIndexResolver,
        limits: DiskANNReadLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNQuery<'_>>> {
        self.selected_source(resolver, control)?
            .map(|source| {
                let parameters = self
                    .index_parameters()
                    .ok_or_else(|| super::invalid("missing index parameters"))?;
                DiskANNQuery::open(self, source, parameters, limits, control)
            })
            .transpose()
    }
}
