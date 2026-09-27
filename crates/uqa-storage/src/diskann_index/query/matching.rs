//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cross-definition graph coverage requires complete raw equality, never a synthetic mutation origin.

use super::{invalid, selection::DiskANNReadSnapshot};
use crate::{
    diskann_index::{
        format::{DiskANNCanonicalOrigin, DiskANNChangeIdentity, DiskANNVectorVersion},
        DiskANNCanonicalRead, DiskANNCanonicalVectorVisitor, DiskANNQueryRead,
    },
    read_control::StorageReadControl,
    vector_index::VectorReadSnapshot,
    StorageBackendResult,
};
use uqa_core::DocId;

pub(super) struct MatchingCanonical {
    actual: DiskANNReadSnapshot,
    values: VectorReadSnapshot,
}

impl MatchingCanonical {
    pub(super) fn new(
        actual: DiskANNReadSnapshot,
        values: VectorReadSnapshot,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        actual.check_control(control)?;
        values.check_control(control)?;
        if actual.dimensions() != values.dimensions() {
            return Err(invalid(
                "fixed canonical vector width differs from selected index",
            ));
        }
        Ok(Self { actual, values })
    }

    fn matching(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNCanonicalOrigin>> {
        self.check_control(control)?;
        let Some(origin) = self.actual.document_origin(document, control)? else {
            return Ok(None);
        };
        if origin.count() != self.values.document_vector_count(document, control)? {
            return Ok(None);
        }
        for ordinal in 0..origin.count() {
            // End the first provider read before starting the second, including when both readers use one physical connection.
            let actual = self
                .actual
                .read_vector(document, ordinal as u32, control)?
                .ok_or_else(|| invalid("missing actual canonical ordinal"))?;
            let selected = self
                .values
                .read_vector(document, ordinal as u32, control)?
                .ok_or_else(|| invalid("missing fixed canonical ordinal"))?;
            if actual.len() != selected.len() {
                return Err(invalid("fixed canonical vector width mismatch"));
            }
            for (actual, selected) in actual.chunks(1024).zip(selected.chunks(1024)) {
                self.check_control(control)?;
                if actual
                    .iter()
                    .zip(selected)
                    .any(|(a, b)| a.to_bits() != b.to_bits())
                {
                    return Ok(None);
                }
            }
        }
        self.check_control(control)?;
        Ok(Some(origin))
    }
}

impl DiskANNCanonicalRead for MatchingCanonical {
    fn corpus_fingerprint(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<[u8; 32]>> {
        self.check_control(control)?;
        self.values.corpus_fingerprint(control)
    }
    fn check_control(&self, control: &StorageReadControl) -> StorageBackendResult<()> {
        self.actual.check_control(control)?;
        self.values.check_control(control)
    }
    fn dimensions(&self) -> u32 {
        self.values.dimensions()
    }
    fn next_document_after(
        &self,
        mut after: Option<DocId>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DocId>> {
        self.check_control(control)?;
        while let Some(document) = self.values.next_document_after(after, control)? {
            if after.is_some_and(|after| document <= after) {
                return Err(invalid("fixed canonical cursor did not advance"));
            }
            if self.matching(document, control)?.is_some() {
                return Ok(Some(document));
            }
            after = Some(document);
        }
        Ok(None)
    }
    fn origin(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNVectorVersion>> {
        Ok(self
            .matching(document, control)?
            .map(DiskANNCanonicalOrigin::version))
    }
    fn visit_document(
        &self,
        document: DocId,
        control: &StorageReadControl,
        visit: &mut DiskANNCanonicalVectorVisitor<'_>,
    ) -> StorageBackendResult<Option<DiskANNVectorVersion>> {
        if self.matching(document, control)?.is_none() {
            return Ok(None);
        }
        self.actual.visit_document(document, control, visit)
    }
    fn read_vector(
        &self,
        document: DocId,
        ordinal: u32,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<uqa_core::memory::BudgetedVec<f32>>> {
        if self.matching(document, control)?.is_none() {
            return Ok(None);
        }
        self.actual.read_vector(document, ordinal, control)
    }
}

impl DiskANNQueryRead for MatchingCanonical {
    fn unversioned_vectors(&self) -> Option<&VectorReadSnapshot> {
        Some(&self.values)
    }
    fn document_origin(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNCanonicalOrigin>> {
        self.matching(document, control)
    }
    fn next_change_after(
        &self,
        mut after: Option<DocId>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNChangeIdentity>> {
        self.check_control(control)?;
        while let Some(change) = self.actual.next_change_after(after, control)? {
            if after.is_some_and(|after| change.document() <= after) {
                return Err(invalid("actual canonical change cursor did not advance"));
            }
            if self.matching(change.document(), control)?.is_some() {
                return Ok(Some(change));
            }
            after = Some(change.document());
        }
        Ok(None)
    }
}
