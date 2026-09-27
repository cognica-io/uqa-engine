//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Document selection combines one physical generation with its retained canonical/change view.

use std::sync::Arc;
use uqa_core::PostingList;

use super::{
    pages::{DiskANNOriginReader, DiskANNPageSource, DiskANNReadLimits, DiskANNReader},
    search::{DiskANNTraversal, DiskANNTraversalStats},
    DiskANNCanonicalRead, DiskANNCanonicalScorer, DiskANNCanonicalVectorVisitor, DiskANNQueryRead,
    ExactVectorReason, NavigationInput,
};
use crate::{
    read_control::StorageReadControl, vector_index::DiskANNIndexParams, StorageBackendResult,
};

mod candidates;
mod matching;
mod retained;
mod selection;
mod statistics;
mod values;
pub use retained::RetainedDiskANNIndex;
pub use selection::{DiskANNReadChanges, DiskANNReadSnapshot};
pub use statistics::DiskANNQueryWork;
#[cfg(test)]
mod tests;

/// Raw canonical document scores and the route that produced their candidate support. PQ estimates never enter the posting payloads; these counters describe logical traversal, not physical I/O.
pub struct DiskANNQueryResult {
    pub postings: PostingList,
    pub exact_reason: Option<ExactVectorReason>,
    pub traversal: DiskANNTraversalStats,
    pub work: DiskANNQueryWork,
}

/// A reusable prepared generation on one fixed canonical/catalog view. Opening verifies resident PQ and complete origins once; later queries reuse those owners and load graph pages lazily. Execution owns logical read observations before invoking this storage algorithm.
pub struct DiskANNQuery<'a> {
    canonical: &'a dyn DiskANNQueryRead,
    reader: DiskANNReader,
    origins: DiskANNOriginReader,
    control: StorageReadControl,
}

impl<'a> DiskANNQuery<'a> {
    /// Open the physical source selected by the caller's actual retained catalog/canonical view. Provider query adapters perform that binding; matching a caller-supplied generation label alone does not establish it. All failures propagate without an exact-scan fallback.
    pub fn open(
        canonical: &'a dyn DiskANNQueryRead,
        source: Arc<dyn DiskANNPageSource>,
        parameters: DiskANNIndexParams,
        limits: DiskANNReadLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        canonical.check_control(control)?;
        let reader = DiskANNReader::open(
            source.clone(),
            canonical.dimensions(),
            parameters,
            limits,
            control,
        )?;
        let origins = DiskANNOriginReader::open(source, limits.max_record_bytes, control)?;
        if reader.manifest() != origins.manifest() {
            return Err(invalid(
                "physical manifest changed while preparing the query",
            ));
        }
        canonical.check_control(control)?;
        Ok(Self {
            canonical,
            reader,
            origins,
            control: control.clone(),
        })
    }

    /// Approximate document top-k after complete tensor reranking, exact uncovered changes and numeric side entries. Exhausted ANN support continues through unexpanded nodes until the document quota is met or the entire base is exhausted.
    pub fn search_knn(
        &self,
        query: &[f32],
        k: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNQueryResult> {
        self.check(control)?;
        let scorer = DiskANNCanonicalScorer::new(self, query, control)?;
        if k == 0 {
            self.check(control)?;
            return Ok(result(
                PostingList::new(),
                None,
                DiskANNTraversalStats::default(),
                DiskANNQueryWork::default(),
            ));
        }
        let navigation =
            match NavigationInput::from_raw(self.canonical.dimensions(), query, control)? {
                NavigationInput::Exact(reason) => {
                    let (postings, exact) =
                        if let Some(source) = self.canonical.unversioned_vectors() {
                            values::exact(&**source, query, k, control)?
                        } else {
                            scorer.search_exact_knn_with_stats(k)?
                        };
                    self.check(control)?;
                    return Ok(result(
                        postings,
                        Some(reason),
                        DiskANNTraversalStats::default(),
                        DiskANNQueryWork {
                            exact,
                            ..DiskANNQueryWork::default()
                        },
                    ));
                }
                NavigationInput::Navigable(navigation) => navigation,
            };
        let mut traversal = DiskANNTraversal::new(self.reader.clone(), &navigation, control)?;
        drop(navigation);
        let mut candidates = candidates::Candidates::new(self, scorer, k, control);
        loop {
            self.check(control)?;
            let nodes = traversal.next_beam()?;
            if nodes.is_empty() {
                break;
            }
            candidates.nodes(&nodes)?;
        }
        candidates.side()?;
        candidates.changes()?;
        if let Some(source) = self.canonical.unversioned_vectors() {
            values::visit(&**source, control, &mut |document| {
                self.check(control)?;
                if self.canonical.origin(document, control)?.is_none() {
                    let (score, vectors) = values::score(&**source, document, query, control)?;
                    if let Some(score) = score {
                        candidates.offer_unversioned(document, score, vectors)?;
                    }
                }
                Ok(())
            })?;
        }
        while candidates.len() < k {
            self.check(control)?;
            let nodes = traversal.complete_next_beam()?;
            if nodes.is_empty() {
                break;
            }
            candidates.nodes(&nodes)?;
        }
        let mut work = candidates.work();
        work.pages = traversal.page_stats();
        let postings = candidates.finish()?;
        self.check(control)?;
        Ok(result(postings, None, traversal.stats(), work))
    }

    /// Exact threshold evaluation streams all visible canonical tensors, including changes and empty replacements, independently of ANN membership.
    pub fn search_threshold(
        &self,
        query: &[f32],
        threshold: f32,
        control: &StorageReadControl,
    ) -> StorageBackendResult<PostingList> {
        self.search_threshold_with_stats(query, threshold, control)
            .map(|result| result.postings)
    }

    /// Return threshold results and scoring work from their one exact invocation, without a second scan.
    pub fn search_threshold_with_stats(
        &self,
        query: &[f32],
        threshold: f32,
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNQueryResult> {
        self.check(control)?;
        let scorer = DiskANNCanonicalScorer::new(self, query, control)?;
        let (postings, exact) = if let Some(source) = self.canonical.unversioned_vectors() {
            values::threshold(&**source, query, threshold, control)?
        } else {
            scorer.search_threshold_with_stats(threshold)?
        };
        self.check(control)?;
        Ok(result(
            postings,
            None,
            DiskANNTraversalStats::default(),
            DiskANNQueryWork {
                exact,
                ..DiskANNQueryWork::default()
            },
        ))
    }

    fn check(&self, control: &StorageReadControl) -> StorageBackendResult<()> {
        self.control.check()?;
        self.canonical.check_control(control)
    }
}

impl DiskANNCanonicalRead for DiskANNQuery<'_> {
    fn check_control(&self, control: &StorageReadControl) -> StorageBackendResult<()> {
        self.check(control)
    }
    fn dimensions(&self) -> u32 {
        self.canonical.dimensions()
    }
    fn next_document_after(
        &self,
        after: Option<uqa_core::DocId>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<uqa_core::DocId>> {
        self.check(control)?;
        let result = self.canonical.next_document_after(after, control)?;
        self.check(control)?;
        Ok(result)
    }
    fn origin(
        &self,
        document: uqa_core::DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<super::format::DiskANNVectorVersion>> {
        self.check(control)?;
        let result = self.canonical.origin(document, control)?;
        self.check(control)?;
        Ok(result)
    }
    fn visit_document(
        &self,
        document: uqa_core::DocId,
        control: &StorageReadControl,
        visit: &mut DiskANNCanonicalVectorVisitor<'_>,
    ) -> StorageBackendResult<Option<super::format::DiskANNVectorVersion>> {
        self.check(control)?;
        let result =
            self.canonical
                .visit_document(document, control, &mut |ordinal, version, raw| {
                    self.check(control)?;
                    visit(ordinal, version, raw)
                })?;
        self.check(control)?;
        Ok(result)
    }
}

fn result(
    postings: PostingList,
    exact_reason: Option<ExactVectorReason>,
    traversal: DiskANNTraversalStats,
    work: DiskANNQueryWork,
) -> DiskANNQueryResult {
    DiskANNQueryResult {
        postings,
        exact_reason,
        traversal,
        work,
    }
}

fn invalid(message: &'static str) -> crate::StorageBackendError {
    crate::mvcc::VersionError::InvalidEncoding(message).into_storage_error()
}
