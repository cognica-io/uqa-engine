//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered canonical observations on a retained provider boundary, independent of graph coverage.

use super::{
    format::{DiskANNCanonicalOrigin, DiskANNChangeIdentity, DiskANNVectorVersion},
    DiskANNCanonicalVectorVisitor,
};
use crate::{mvcc::VersionError, read_control::StorageReadControl, StorageBackendResult};
use uqa_core::DocId;

/// Borrow one canonical ordinal in document order. Failure invalidates the consumer's partial output.
pub type DiskANNCanonicalCorpusVisitor<'a> =
    dyn FnMut(DocId, u32, DiskANNVectorVersion, &[f32]) -> StorageBackendResult<()> + 'a;

/// A retained canonical source with the actual versioned change journal on that same view. The lifecycle owner must establish complete coverage before opening a query; absence from this journal alone is not proof of build membership.
pub trait DiskANNQueryRead: DiskANNCanonicalRead {
    /// Count visible ordinals from complete origin metadata without decoding coordinates or preparing a graph reader.
    fn vector_count(&self, control: &StorageReadControl) -> StorageBackendResult<usize> {
        self.check_control(control)?;
        let mut after = None;
        let mut count = 0_usize;
        while let Some(document) = self.next_document_after(after, control)? {
            self.check_control(control)?;
            if after.is_some_and(|previous| document <= previous) {
                return Err(VersionError::InvalidEncoding(
                    "canonical vector count cursor did not advance",
                )
                .into_storage_error());
            }
            let origin = self.document_origin(document, control)?.ok_or_else(|| {
                VersionError::InvalidEncoding("enumerated vector document has no origin")
                    .into_storage_error()
            })?;
            let vectors = usize::try_from(origin.count())
                .map_err(|_| uqa_core::memory::MemoryError::SizeOverflow)?;
            count = count
                .checked_add(vectors)
                .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
            after = Some(document);
        }
        self.check_control(control)?;
        Ok(count)
    }

    /// Test nonempty membership using complete canonical metadata on this exact view.
    fn contains_vectors(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<bool> {
        self.check_control(control)?;
        let origin = self.document_origin(document, control)?;
        self.check_control(control)?;
        Ok(origin.is_some_and(|origin| origin.count() != 0))
    }

    /// Return the selected mutation and complete tensor cardinality after validating its ordinal-key coverage. This metadata probe must not decode coordinate values or inspect graph pages; empty replacements return Some with count zero.
    fn document_origin(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNCanonicalOrigin>>;

    /// Return current journaled documents in strictly increasing order, including empty replacements. Validate the immutable change envelope against its current canonical origin; skip obsolete versions without loading their values.
    fn next_change_after(
        &self,
        after: Option<DocId>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNChangeIdentity>>;
}

/// One fixed committed/private canonical source. Implementations retain their original visibility and controls; callbacks must not reenter the source.
pub trait DiskANNCanonicalRead {
    /// Check the retained source's original controls and the invoking control, including queries that need no provider reads.
    fn check_control(&self, control: &StorageReadControl) -> StorageBackendResult<()>;

    /// The canonical width, shared by every ordinal on this source.
    fn dimensions(&self) -> u32;

    /// Return the least live document strictly after `after` from the union of canonical keys and origin keys. Include empty replacements and unstamped canonical values; read key metadata only, with bounded workspace.
    fn next_document_after(
        &self,
        after: Option<DocId>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DocId>>;

    /// Validate the complete ordinal set before returning its origin. Absence and an explicit empty replacement remain distinct.
    fn origin(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNVectorVersion>>;

    /// Stream every visible ordinal, preserving coordinate bits and checking the complete tensor's origin and shape. Use bounded point reads and a reusable charged vector buffer.
    fn visit_document(
        &self,
        document: DocId,
        control: &StorageReadControl,
        visit: &mut DiskANNCanonicalVectorVisitor<'_>,
    ) -> StorageBackendResult<Option<DiskANNVectorVersion>>;

    /// Stream the entire fixed source in document/ordinal order without retaining a corpus directory. Empty replacements are validated but emit no vectors; unstamped values fail instead of disappearing from the corpus.
    fn visit_all(
        &self,
        control: &StorageReadControl,
        visit: &mut DiskANNCanonicalCorpusVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.check_control(control)?;
        let mut after = None;
        while let Some(document) = self.next_document_after(after, control)? {
            control.check()?;
            if after.is_some_and(|previous| document <= previous) {
                return Err(VersionError::InvalidEncoding(
                    "canonical corpus cursor did not advance",
                )
                .into_storage_error());
            }
            let origin = self.visit_document(document, control, &mut |ordinal, version, raw| {
                visit(document, ordinal, version, raw)
            })?;
            if origin.is_none() {
                return Err(VersionError::InvalidEncoding(
                    "enumerated canonical document has no origin",
                )
                .into_storage_error());
            }
            after = Some(document);
        }
        self.check_control(control)
    }
}
