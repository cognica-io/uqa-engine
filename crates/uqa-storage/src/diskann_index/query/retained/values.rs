//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A raw capability keeps every enclosing physical owner and its original controls.

use super::Retained;
use crate::{
    diskann_index::DiskANNQueryRead, read_control::StorageReadControl, vector_index::VectorRead,
    StorageBackendResult,
};
use uqa_core::{memory::BudgetedVec, DocId};

impl<S: DiskANNQueryRead + Send + Sync> VectorRead for Retained<S> {
    fn corpus_fingerprint(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<[u8; 32]>> {
        self.check_control(control)?;
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
        self.check_control(control)?;
        match self.canonical.unversioned_vectors() {
            Some(source) => source.next_document_after(after, control),
            None => self.canonical.next_document_after(after, control),
        }
    }
    fn document_vector_count(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<u64> {
        self.check_control(control)?;
        match self.canonical.unversioned_vectors() {
            Some(source) => source.document_vector_count(document, control),
            None => Ok(self.canonical.document_origin(document, control)?.map_or(
                0,
                crate::diskann_index::format::DiskANNCanonicalOrigin::count,
            )),
        }
    }
    fn read_vector(
        &self,
        document: DocId,
        ordinal: u32,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<BudgetedVec<f32>>> {
        self.check_control(control)?;
        match self.canonical.unversioned_vectors() {
            Some(source) => source.read_vector(document, ordinal, control),
            None => self.canonical.read_vector(document, ordinal, control),
        }
    }
}
