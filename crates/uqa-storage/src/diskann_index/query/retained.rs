//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Owned vector-index snapshots keep canonical visibility and prepared physical readers together.

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
use uqa_core::{
    memory::{Budgeted, MemoryError},
    DocId, PostingList,
};

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
        let mut after = None;
        let mut count = 0_usize;
        while let Some(document) = self
            .retained
            .canonical
            .next_document_after(after, &self.retained.control)?
        {
            self.check()?;
            if after.is_some_and(|previous| document <= previous) {
                return Err(invalid("retained vector count cursor did not advance"));
            }
            let origin = self
                .retained
                .canonical
                .document_origin(document, &self.retained.control)?
                .ok_or_else(|| invalid("enumerated vector document has no origin"))?;
            let vectors = usize::try_from(origin.count()).map_err(|_| MemoryError::SizeOverflow)?;
            count = count
                .checked_add(vectors)
                .ok_or(MemoryError::SizeOverflow)?;
            after = Some(document);
        }
        self.check()?;
        Ok(count)
    }

    fn contains_document(&self, document: DocId) -> StorageBackendResult<bool> {
        self.check()?;
        let origin = self
            .retained
            .canonical
            .document_origin(document, &self.retained.control)?;
        self.check()?;
        Ok(origin.is_some_and(|origin| origin.count() != 0))
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
}

fn read_only() -> StorageBackendError {
    StorageBackendError::Other("cannot write a retained DiskANN snapshot".into())
}
