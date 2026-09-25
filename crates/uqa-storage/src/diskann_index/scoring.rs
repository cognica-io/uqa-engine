//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical tensor scores precede document selection and posting construction.

use super::{format::DiskANNVectorVersion, DiskANNCanonicalRead};
use crate::{
    mvcc::VersionError,
    read_control::StorageReadControl,
    vector_index::{
        cosine_similarity_controlled,
        query::{postings_from_unique_scores, VectorQueryBuffer},
        validate_threshold, validate_vector_values_controlled,
    },
    StorageBackendResult,
};
use uqa_core::{DocId, PostingList};

mod selection;
#[cfg(test)]
mod tests;

/// One vector-bearing document scored from every ordinal on a fixed canonical source. This is raw cosine, not navigation distance or probability.
#[derive(Debug, Clone, Copy)]
pub struct DiskANNDocumentScore {
    document: DocId,
    version: DiskANNVectorVersion,
    vectors: u64,
    score: f32,
}

impl DiskANNDocumentScore {
    /// Canonical document identity before posting construction.
    pub fn document(&self) -> DocId {
        self.document
    }
    /// Original mutation version shared by every scored ordinal.
    pub fn version(&self) -> DiskANNVectorVersion {
        self.version
    }
    /// Number of visible tensor ordinals, independent of the document candidate quota.
    pub fn vector_count(&self) -> u64 {
        self.vectors
    }
    /// Canonical sequential-f32 cosine maximum, including its existing numeric edge behavior.
    pub fn raw_cosine(&self) -> f32 {
        self.score
    }
}

/// Borrows a validated query and one retained canonical boundary. Exact scans serve explicit exact/threshold and numeric-edge routes; they do not replace missing ANN coverage or recover from graph errors.
pub struct DiskANNCanonicalScorer<'a> {
    source: &'a dyn DiskANNCanonicalRead,
    query: &'a [f32],
    control: &'a StorageReadControl,
}

impl<'a> DiskANNCanonicalScorer<'a> {
    pub fn new(
        source: &'a dyn DiskANNCanonicalRead,
        query: &'a [f32],
        control: &'a StorageReadControl,
    ) -> StorageBackendResult<Self> {
        source.check_control(control)?;
        if source.dimensions() == 0 {
            return Err(invalid("canonical scoring requires positive dimensions"));
        }
        validate_vector_values_controlled(source.dimensions(), query, Some(control))?;
        source.check_control(control)?;
        Ok(Self {
            source,
            query,
            control,
        })
    }

    /// Score the complete tensor using the canonical total-order maximum. Absent documents and explicit empty tensors have no score.
    pub fn score_document(
        &self,
        document: DocId,
    ) -> StorageBackendResult<Option<DiskANNDocumentScore>> {
        self.document(document).map(|(_, score)| score)
    }

    /// Validate a physical candidate's original version and ordinal before admitting its complete document score. Stale versions are masked; a matching origin with an impossible ordinal is corrupt.
    pub fn score_candidate(
        &self,
        document: DocId,
        ordinal: u32,
        version: DiskANNVectorVersion,
    ) -> StorageBackendResult<Option<DiskANNDocumentScore>> {
        self.source.check_control(self.control)?;
        if self.source.origin(document, self.control)? != Some(version) {
            self.source.check_control(self.control)?;
            return Ok(None);
        }
        let (actual, score) = self.document(document)?;
        if actual != Some(version) {
            return Err(invalid("canonical origin changed on a retained source"));
        }
        let score = score
            .filter(|score| u64::from(ordinal) < score.vectors)
            .ok_or_else(|| invalid("candidate ordinal is outside its canonical tensor"))?;
        Ok(Some(score))
    }

    /// Stream one score per vector-bearing document in ascending document order. On any error, the caller must discard all output already produced by its callback.
    pub fn visit_scores(
        &self,
        visit: &mut dyn FnMut(DiskANNDocumentScore) -> StorageBackendResult<()>,
    ) -> StorageBackendResult<()> {
        self.source.check_control(self.control)?;
        let mut after = None;
        while let Some(document) = self.source.next_document_after(after, self.control)? {
            if after.is_some_and(|previous| document <= previous) {
                return Err(invalid("canonical scoring cursor did not advance"));
            }
            let (version, score) = self.document(document)?;
            if version.is_none() {
                return Err(invalid("enumerated canonical document has no origin"));
            }
            if let Some(score) = score {
                visit(score)?;
            }
            after = Some(document);
        }
        self.source.check_control(self.control)
    }

    /// Exact document top-k with workspace proportional to selected documents, not corpus size. Scores descend with document-ID tie breaking; returned postings retain their ordinary document order and caller-owned result boundary.
    pub fn search_exact_knn(&self, k: usize) -> StorageBackendResult<PostingList> {
        self.source.check_control(self.control)?;
        if k == 0 {
            return Ok(PostingList::new());
        }
        let mut selected = selection::TopK::new(k, self.control);
        self.visit_scores(&mut |score| selected.offer(score))?;
        let result = selected.finish(self.control)?;
        self.source.check_control(self.control)?;
        Ok(result)
    }

    /// Scan all canonical documents and apply the existing raw-cosine threshold after full tensor reduction. NaN comparison and finite-threshold validation match the existing exact index.
    pub fn search_threshold(&self, threshold: f32) -> StorageBackendResult<PostingList> {
        self.source.check_control(self.control)?;
        validate_threshold(threshold)?;
        let mut scores = VectorQueryBuffer::new(Some(self.control));
        self.visit_scores(&mut |score| {
            if score.score >= threshold {
                scores.push((score.document, score.score))?;
            }
            Ok(())
        })?;
        let result = postings_from_unique_scores(scores, None, Some(self.control))?;
        self.source.check_control(self.control)?;
        Ok(result)
    }

    fn document(
        &self,
        document: DocId,
    ) -> StorageBackendResult<(Option<DiskANNVectorVersion>, Option<DiskANNDocumentScore>)> {
        self.source.check_control(self.control)?;
        let mut version = None;
        let mut best: Option<f32> = None;
        let mut count = 0_u64;
        let mut failure = None;
        let returned =
            self.source
                .visit_document(document, self.control, &mut |ordinal, current, raw| {
                    if failure.is_none() {
                        let result = (|| {
                            self.source.check_control(self.control)?;
                            if u64::from(ordinal) != count
                                || version.is_some_and(|version| version != current)
                            {
                                return Err(invalid(
                                    "canonical tensor identity changed while scoring",
                                ));
                            }
                            validate_vector_values_controlled(
                                self.source.dimensions(),
                                raw,
                                Some(self.control),
                            )?;
                            let score = cosine_similarity_controlled(self.query, raw, || {
                                self.source.check_control(self.control)
                            })?;
                            if best.is_none_or(|best| score.total_cmp(&best).is_gt()) {
                                best = Some(score);
                            }
                            version = Some(current);
                            count += 1;
                            Ok(())
                        })();
                        if let Err(error) = result {
                            failure = Some(error);
                        }
                    }
                    if failure.is_some() {
                        Err(invalid("canonical score consumer rejected an ordinal"))
                    } else {
                        Ok(())
                    }
                });
        if let Some(error) = failure {
            return Err(error);
        }
        let returned = returned?;
        self.source.check_control(self.control)?;
        if count != 0 && returned != version {
            return Err(invalid("canonical tensor returned a different origin"));
        }
        let score = best.map(|score| DiskANNDocumentScore {
            document,
            version: version.expect("a scored ordinal has an origin"),
            vectors: count,
            score,
        });
        Ok((returned, score))
    }
}

fn invalid(message: &'static str) -> crate::StorageBackendError {
    VersionError::InvalidEncoding(message).into_storage_error()
}
