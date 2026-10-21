//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical `DiskANN` scores use the common bounded document selection owner.

use super::DiskANNDocumentScore;
use crate::{read_control::StorageReadControl, vector_index::query, StorageBackendResult};
use uqa_core::{DocId, PostingList};

pub(in crate::diskann_index) struct TopK(query::TopK);

impl TopK {
    pub(in crate::diskann_index) fn new(limit: usize, control: &StorageReadControl) -> Self {
        Self(query::TopK::new(limit, control))
    }
    pub(in crate::diskann_index) fn deduplicating(
        limit: usize,
        control: &StorageReadControl,
    ) -> Self {
        Self(query::TopK::deduplicating(limit, control))
    }
    pub(in crate::diskann_index) fn len(&self) -> usize {
        self.0.len()
    }
    pub(in crate::diskann_index) fn offer(
        &mut self,
        score: DiskANNDocumentScore,
    ) -> StorageBackendResult<()> {
        self.0.offer_raw(score.document, score.score)
    }
    pub(in crate::diskann_index) fn offer_raw(
        &mut self,
        document: DocId,
        score: f32,
    ) -> StorageBackendResult<()> {
        self.0.offer_raw(document, score)
    }
    pub(in crate::diskann_index) fn finish(
        self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<PostingList> {
        self.0.finish(control)
    }
}
