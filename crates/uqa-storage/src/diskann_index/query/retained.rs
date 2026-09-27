//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Owned vector-index snapshots keep canonical visibility and prepared physical readers together.

use super::selection::{DiskANNReadChanges, DiskANNReadSnapshot, SelectedCanonical};
use super::{invalid, DiskANNQuery};
use crate::diskann_index::{
    format::DiskANNManifest,
    pages::{DiskANNOriginReader, DiskANNPageSource, DiskANNReadLimits, DiskANNReader},
    DiskANNQueryRead,
};
use crate::{
    read_control::StorageReadControl, vector_index::DiskANNIndexParams, StorageBackendError,
    StorageBackendResult, VectorIndex,
};
use std::sync::Arc;
use uqa_core::{memory::Budgeted, DocId, PostingList};

mod values;

struct Retained<S> {
    canonical: S,
    reader: DiskANNReader,
    origins: DiskANNOriginReader,
    control: StorageReadControl,
}

/// Read-only `VectorIndex` over an owned fixed canonical source and its prepared generation. Cloning and nested snapshots share the same leases, resident data and original query allowance, without reopening pages or copying the canonical corpus.
pub struct RetainedDiskANNIndex<S> {
    retained: Arc<Budgeted<Retained<S>>>,
}

impl<S> Clone for RetainedDiskANNIndex<S> {
    fn clone(&self) -> Self {
        Self {
            retained: Arc::clone(&self.retained),
        }
    }
}

impl<S: DiskANNQueryRead + Send + Sync + 'static> RetainedDiskANNIndex<S> {
    /// Prepare the source already selected on the supplied canonical/catalog view, then retain both owners. Provider adapters establish this actual association; a matching generation label by itself does not establish visibility.
    pub fn open(
        canonical: S,
        source: Arc<dyn DiskANNPageSource>,
        parameters: DiskANNIndexParams,
        limits: DiskANNReadLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        let DiskANNQuery {
            reader,
            origins,
            control: original,
            ..
        } = DiskANNQuery::open(&canonical, source, parameters, limits, control)?;
        let retained = Retained {
            canonical,
            reader,
            origins,
            control: original,
        };
        let retained =
            Budgeted::new(retained, control.memory().empty_reservation()).into_shared()?;
        control.check()?;
        Ok(Self { retained })
    }

    /// Immutable physical identity and effective configuration from the actual selected generation. Reading metadata does not perform a logical vector query.
    pub fn manifest(&self) -> &DiskANNManifest {
        self.retained.reader.manifest()
    }

    pub(in crate::diskann_index) fn canonical(&self) -> &S {
        &self.retained.canonical
    }

    /// Advance canonical visibility within the same storage-owned lineage while sharing its prepared physical generation. Only the lifecycle owner may establish this association.
    pub(in crate::diskann_index) fn with_canonical(
        &self,
        canonical: S,
    ) -> StorageBackendResult<Self> {
        self.check()?;
        canonical.check_control(&self.retained.control)?;
        if canonical.dimensions() != self.dimensions() {
            return Err(invalid("replacement canonical dimensions differ"));
        }
        let retained = Retained {
            canonical,
            reader: self.retained.reader.clone(),
            origins: self.retained.origins.clone(),
            control: self.retained.control.clone(),
        };
        let retained = Budgeted::new(retained, self.retained.control.memory().empty_reservation())
            .into_shared()?;
        self.check()?;
        Ok(Self { retained })
    }

    fn check(&self) -> StorageBackendResult<()> {
        self.retained.control.check()?;
        self.retained
            .canonical
            .check_control(&self.retained.control)
    }

    fn query(&self, invoking: &StorageReadControl) -> DiskANNQuery<'_> {
        // The search uses the original memory allowance. This additional control is checked throughout scoring/traversal so an independent invocation can cancel without replacing retained ownership.
        DiskANNQuery {
            canonical: &self.retained.canonical,
            reader: self.retained.reader.clone(),
            origins: self.retained.origins.clone(),
            control: invoking.clone(),
        }
    }
}

impl<S: DiskANNQueryRead + Send + Sync + 'static> VectorIndex for RetainedDiskANNIndex<S> {
    fn dimensions(&self) -> u32 {
        self.retained.canonical.dimensions()
    }
    fn index_kind(&self) -> &'static str {
        "diskann"
    }
    fn diskann_query_metadata(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<crate::diskann_index::DiskANNQueryMetadata>> {
        self.check()?;
        control.check()?;
        let corpus_fingerprint = self.retained.canonical.corpus_fingerprint(control)?;
        self.check()?;
        Ok(Some(crate::diskann_index::DiskANNQueryMetadata {
            manifest: *self.manifest(),
            corpus_fingerprint,
            read_limits: self.retained.reader.limits(),
            read_capabilities: self.retained.reader.capabilities(),
        }))
    }
    fn add(&mut self, _: DocId, _: Vec<f32>) -> StorageBackendResult<()> {
        Err(read_only())
    }
    fn add_many(&mut self, _: DocId, _: Vec<Vec<f32>>) -> StorageBackendResult<()> {
        Err(read_only())
    }
    fn delete(&mut self, _: DocId) -> StorageBackendResult<()> {
        Err(read_only())
    }
    fn clear(&mut self) -> StorageBackendResult<()> {
        Err(read_only())
    }
    fn initialize(&mut self) -> StorageBackendResult<()> {
        Err(read_only())
    }

    fn search_knn(&self, query: &[f32], k: usize) -> StorageBackendResult<PostingList> {
        self.search_knn_with_control(query, k, &self.retained.control)
    }
    fn search_threshold(&self, query: &[f32], threshold: f32) -> StorageBackendResult<PostingList> {
        self.search_threshold_with_control(query, threshold, &self.retained.control)
    }
    fn search_knn_with_control(
        &self,
        query: &[f32],
        k: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<PostingList> {
        Ok(self
            .query(control)
            .search_knn(query, k, &self.retained.control)?
            .postings)
    }
    fn search_threshold_with_control(
        &self,
        query: &[f32],
        threshold: f32,
        control: &StorageReadControl,
    ) -> StorageBackendResult<PostingList> {
        self.query(control)
            .search_threshold(query, threshold, &self.retained.control)
    }

    fn count(&self) -> StorageBackendResult<usize> {
        self.check()?;
        if let Some(source) = self.retained.canonical.unversioned_vectors() {
            return super::values::count(&**source, &self.retained.control);
        }
        let count = self
            .retained
            .canonical
            .vector_count(&self.retained.control)?;
        self.check()?;
        Ok(count)
    }

    fn contains_document(&self, document: DocId) -> StorageBackendResult<bool> {
        self.check()?;
        if let Some(source) = self.retained.canonical.unversioned_vectors() {
            return source
                .document_vector_count(document, &self.retained.control)
                .map(|count| count != 0);
        }
        let contains = self
            .retained
            .canonical
            .contains_vectors(document, &self.retained.control)?;
        self.check()?;
        Ok(contains)
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn VectorIndex>> {
        self.check()?;
        Ok(Arc::new(self.clone()))
    }
    fn snapshot_with_control(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Arc<dyn VectorIndex>> {
        control.check()?;
        self.snapshot()
    }

    fn diskann_read_snapshot(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNReadSnapshot>> {
        control.check()?;
        self.check()?;
        if self.retained.canonical.unversioned_vectors().is_some() {
            return Ok(None);
        }
        Ok(Some(DiskANNReadSnapshot::new(
            self.retained.clone(),
            self.manifest().input().generation,
            &self.retained.control,
        )))
    }

    fn vector_read_snapshot(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<crate::vector_index::VectorReadSnapshot>> {
        control.check()?;
        self.check()?;
        Ok(Some(self.retained.clone()))
    }

    fn snapshot_with_vector_read(
        &self,
        source: crate::vector_index::VectorReadSnapshot,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<Arc<dyn VectorIndex>>> {
        self.check()?;
        let actual = DiskANNReadSnapshot::new(
            self.retained.clone(),
            self.manifest().input().generation,
            &self.retained.control,
        );
        let canonical = super::matching::MatchingCanonical::new(actual, source, control)?;
        let retained = Retained {
            canonical,
            reader: self.retained.reader.clone(),
            origins: self.retained.origins.clone(),
            control: self.retained.control.clone(),
        };
        let retained = Budgeted::new(retained, self.retained.control.memory().empty_reservation())
            .into_shared()?;
        self.check()?;
        control.check()?;
        Ok(Some(Arc::new(RetainedDiskANNIndex { retained })))
    }

    fn snapshot_with_diskann_changes(
        &self,
        changes: &DiskANNReadChanges,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<Arc<dyn VectorIndex>>> {
        control.check()?;
        self.check()?;
        if self.retained.canonical.unversioned_vectors().is_some() {
            return Ok(None);
        }
        let base = DiskANNReadSnapshot::new(
            self.retained.clone(),
            self.manifest().input().generation,
            &self.retained.control,
        );
        let canonical = SelectedCanonical::new(base, changes, control)?;
        let retained = Retained {
            canonical,
            reader: self.retained.reader.clone(),
            origins: self.retained.origins.clone(),
            control: self.retained.control.clone(),
        };
        let retained = Budgeted::new(retained, self.retained.control.memory().empty_reservation())
            .into_shared()?;
        self.check()?;
        control.check()?;
        Ok(Some(Arc::new(RetainedDiskANNIndex { retained })))
    }
}

impl<S: DiskANNQueryRead> crate::diskann_index::DiskANNCanonicalRead for Budgeted<Retained<S>> {
    fn corpus_fingerprint(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<[u8; 32]>> {
        self.control.check()?;
        self.canonical.corpus_fingerprint(control)
    }
    fn check_control(&self, control: &StorageReadControl) -> StorageBackendResult<()> {
        self.control.check()?;
        self.canonical.check_control(control)
    }
    fn dimensions(&self) -> u32 {
        self.canonical.dimensions()
    }
    fn next_document_after(
        &self,
        after: Option<DocId>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DocId>> {
        self.canonical.next_document_after(after, control)
    }
    fn origin(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<crate::diskann_index::format::DiskANNVectorVersion>> {
        self.canonical.origin(document, control)
    }
    fn visit_document(
        &self,
        document: DocId,
        control: &StorageReadControl,
        visit: &mut crate::diskann_index::DiskANNCanonicalVectorVisitor<'_>,
    ) -> StorageBackendResult<Option<crate::diskann_index::format::DiskANNVectorVersion>> {
        self.canonical.visit_document(document, control, visit)
    }

    fn read_vector(
        &self,
        document: DocId,
        ordinal: u32,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<uqa_core::memory::BudgetedVec<f32>>> {
        self.control.check()?;
        self.canonical.read_vector(document, ordinal, control)
    }
}

impl<S: DiskANNQueryRead> DiskANNQueryRead for Budgeted<Retained<S>> {
    fn document_origin(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<crate::diskann_index::format::DiskANNCanonicalOrigin>> {
        self.canonical.document_origin(document, control)
    }
    fn next_change_after(
        &self,
        after: Option<DocId>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<crate::diskann_index::format::DiskANNChangeIdentity>> {
        self.canonical.next_change_after(after, control)
    }
}

fn read_only() -> StorageBackendError {
    StorageBackendError::Other("cannot write a retained DiskANN snapshot".into())
}
