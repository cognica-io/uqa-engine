//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;

use sha2::{Digest, Sha256};
use uqa_core::{memory::Budgeted, DocId};

use super::{invalid, read_record, DiskANNPageSource, DiskANNRecordKey};
use crate::diskann_index::format::{
    DiskANNCanonicalOrigin, DiskANNManifest, DiskANNOriginLayout, DiskANNOriginSummary,
    ORIGIN_BATCH_DOCUMENTS,
};
use crate::{read_control::StorageReadControl, StorageBackendResult};

pub(super) struct OriginVerifier {
    layout: DiskANNOriginLayout,
    summary: DiskANNOriginSummary,
    expected_vectors: u64,
    next: u64,
    previous: Option<DocId>,
    vectors: u64,
    hash: Sha256,
}

impl OriginVerifier {
    pub(super) fn new(manifest: &DiskANNManifest) -> StorageBackendResult<Self> {
        let summary = manifest
            .origins()
            .ok_or_else(|| invalid("generation has no complete origin artifact"))?;
        let input = manifest.input();
        Ok(Self {
            layout: DiskANNOriginLayout::new(
                input.generation,
                input.dimensions,
                summary.documents(),
            )?,
            summary,
            expected_vectors: input.coverage.vector_count(),
            next: 0,
            previous: None,
            vectors: 0,
            hash: Sha256::new(),
        })
    }

    pub(super) fn batch(
        &mut self,
        first: u64,
        bytes: &[u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        if first != self.next {
            return Err(invalid("noncontiguous origin batches"));
        }
        let batch = self.layout.decode(first, bytes, control)?;
        for index in 0..batch.len() {
            control.check()?;
            let entry = batch.entry(index).expect("validated origin batch");
            if self.previous.is_some_and(|last| last >= entry.document()) {
                return Err(invalid("origin order crosses a batch boundary"));
            }
            self.vectors = self
                .vectors
                .checked_add(entry.origin().count())
                .ok_or_else(|| invalid("origin vector count overflow"))?;
            self.previous = Some(entry.document());
        }
        for chunk in batch.bytes().chunks(4096) {
            control.check()?;
            self.hash.update(chunk);
        }
        self.next += batch.len() as u64;
        Ok(())
    }

    pub(super) fn finish(self) -> StorageBackendResult<()> {
        if self.next != self.summary.documents()
            || self.vectors != self.expected_vectors
            || <[u8; 32]>::from(self.hash.finalize()) != self.summary.digest()
        {
            return Err(invalid(
                "origin artifact is incomplete or differs from manifest",
            ));
        }
        Ok(())
    }
}

struct Retained {
    source: Arc<dyn DiskANNPageSource>,
    manifest: DiskANNManifest,
    layout: DiskANNOriginLayout,
    documents: u64,
    maximum: usize,
}

/// Verified complete origin stream on one immutable physical generation. This reader needs neither resident PQ codes nor the original canonical snapshot; publication authority still belongs to the catalog lifecycle owner.
#[derive(Clone)]
pub struct DiskANNOriginReader {
    retained: Arc<Budgeted<Retained>>,
}

impl DiskANNOriginReader {
    /// Check every ordered origin batch, total tensor cardinality and stream digest before retaining a lookup handle. Missing/corrupt data is an error, never negative membership.
    pub fn open(
        source: Arc<dyn DiskANNPageSource>,
        maximum: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        let bytes = read_record(&*source, DiskANNRecordKey::Manifest, maximum, control)?;
        let manifest = DiskANNManifest::decode(source.generation(), &bytes, control)?;
        drop(bytes);
        let mut verifier = OriginVerifier::new(&manifest)?;
        let documents = verifier.summary.documents();
        let layout = verifier.layout;
        while verifier.next < documents {
            let first = verifier.next;
            let bytes = read_record(
                &*source,
                DiskANNRecordKey::Origins(first),
                maximum.min(DiskANNOriginLayout::MAX_ENCODED_BYTES),
                control,
            )?;
            verifier.batch(first, &bytes, control)?;
        }
        verifier.finish()?;
        control.check()?;
        let retained = Retained {
            source,
            manifest,
            layout,
            documents,
            maximum,
        };
        let retained =
            Budgeted::new(retained, control.memory().empty_reservation()).into_shared()?;
        Ok(Self { retained })
    }

    pub fn manifest(&self) -> &DiskANNManifest {
        &self.retained.manifest
    }

    /// Binary search fixed-capacity batches with one charged decoded record at a time. An explicit empty tensor returns Some with count zero; an absent document returns None.
    pub fn origin(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNCanonicalOrigin>> {
        control.check()?;
        if self.retained.source.generation() != self.manifest().input().generation {
            return Err(invalid("origin source changed its retained generation"));
        }
        let mut low = 0;
        let mut high = self
            .retained
            .documents
            .div_ceil(ORIGIN_BATCH_DOCUMENTS as u64);
        while low < high {
            control.check()?;
            let middle = low + (high - low) / 2;
            let first = middle * ORIGIN_BATCH_DOCUMENTS as u64;
            let bytes = read_record(
                &*self.retained.source,
                DiskANNRecordKey::Origins(first),
                self.retained
                    .maximum
                    .min(DiskANNOriginLayout::MAX_ENCODED_BYTES),
                control,
            )?;
            let batch = self.retained.layout.decode(first, &bytes, control)?;
            if document < batch.entry(0).expect("nonempty batch").document() {
                high = middle;
            } else if document
                > batch
                    .entry(batch.len() - 1)
                    .expect("nonempty batch")
                    .document()
            {
                low = middle + 1;
            } else {
                for index in 0..batch.len() {
                    control.check()?;
                    let entry = batch.entry(index).expect("validated batch");
                    if entry.document() == document {
                        return Ok(Some(entry.origin()));
                    }
                }
                break;
            }
        }
        control.check()?;
        Ok(None)
    }
}
