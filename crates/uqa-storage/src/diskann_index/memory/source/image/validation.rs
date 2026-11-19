//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical populations and sealed origins advance independently of corpus cardinality.

use super::{
    invalid, Canonical, DiskANNCanonicalCounts, DiskANNGeneration, DiskANNMemorySource, Tensor,
};
use crate::{
    diskann_index::{
        format::{DiskANNOriginBatch, DiskANNOriginEntry, DiskANNVectorVersion},
        pages::ImageOrigins,
    },
    image_io::ImagePreparation,
    read_control::StorageReadControl,
    StorageBackendResult,
};
use uqa_core::{
    memory::{BudgetedSharedMapIter, MemoryError},
    DocId,
};

enum Stage {
    Generation,
    Documents,
    Changes,
    Batch,
    Origin,
    Merge(DiskANNOriginEntry),
    Trailing,
    Complete,
}

pub(in crate::diskann_index::memory) struct CanonicalImageValidation<'a> {
    source: &'a Canonical,
    generation: DiskANNGeneration,
    documents: BudgetedSharedMapIter<'a, DocId, Tensor>,
    changes: BudgetedSharedMapIter<'a, DocId, DiskANNVectorVersion>,
    merge: BudgetedSharedMapIter<'a, DocId, Tensor>,
    origins: ImageOrigins<'a>,
    batch: Option<DiskANNOriginBatch<'a>>,
    position: usize,
    current: u64,
    changed: u64,
    revision_seen: bool,
    stage: Stage,
}

impl<'a> CanonicalImageValidation<'a> {
    pub(in crate::diskann_index::memory) fn new(
        source: &'a Canonical,
        physical: &'a DiskANNMemorySource,
        generation: DiskANNGeneration,
    ) -> StorageBackendResult<Self> {
        Ok(Self {
            source,
            generation,
            documents: source.documents.iter(),
            changes: source.changes.iter(),
            merge: source.documents.iter(),
            origins: physical.image_origins()?,
            batch: None,
            position: 0,
            current: 0,
            changed: 0,
            revision_seen: source.revision.is_none() && source.documents.is_empty(),
            stage: Stage::Generation,
        })
    }

    pub(in crate::diskann_index::memory) fn check(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        self.source.control.check()?;
        control.check()
    }

    pub(in crate::diskann_index::memory) fn advance(
        &mut self,
        maximum: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<ImagePreparation> {
        self.check(control)?;
        let mut positions = 0;
        while positions < maximum && !matches!(self.stage, Stage::Complete) {
            self.check(control)?;
            self.step(control)?;
            positions += 1;
        }
        self.check(control)?;
        Ok(ImagePreparation {
            positions,
            ready: matches!(self.stage, Stage::Complete),
        })
    }

    fn step(&mut self, control: &StorageReadControl) -> StorageBackendResult<()> {
        self.stage = match std::mem::replace(&mut self.stage, Stage::Complete) {
            Stage::Generation => {
                if self.source.covered_generation != Some(self.generation) {
                    return Err(invalid());
                }
                Stage::Documents
            }
            Stage::Documents => self.document()?,
            Stage::Changes => {
                if let Some((document, _)) = self.changes.next() {
                    if self.source.documents.get(document).is_none() {
                        return Err(invalid());
                    }
                    Stage::Changes
                } else {
                    Stage::Batch
                }
            }
            Stage::Batch => {
                self.batch = self.origins.next_batch(control)?;
                self.position = 0;
                if self.batch.is_some() {
                    Stage::Origin
                } else {
                    Stage::Trailing
                }
            }
            Stage::Origin => {
                if let Some(base) = self
                    .batch
                    .as_ref()
                    .and_then(|batch| batch.entry(self.position))
                {
                    self.position += 1;
                    Stage::Merge(base)
                } else {
                    Stage::Batch
                }
            }
            Stage::Merge(base) => {
                let (document, tensor) = self.merge.next().ok_or_else(invalid)?;
                let changed = self.source.changes.get(document).is_some();
                if *document < base.document() {
                    if !changed {
                        return Err(invalid());
                    }
                    Stage::Merge(base)
                } else {
                    if *document != base.document() || (tensor.origin != base.origin()) != changed {
                        return Err(invalid());
                    }
                    Stage::Origin
                }
            }
            Stage::Trailing => {
                if let Some((document, _)) = self.merge.next() {
                    if self.source.changes.get(document).is_none() {
                        return Err(invalid());
                    }
                    Stage::Trailing
                } else {
                    Stage::Complete
                }
            }
            Stage::Complete => Stage::Complete,
        };
        Ok(())
    }

    fn document(&mut self) -> StorageBackendResult<Stage> {
        let Some((&document, tensor)) = self.documents.next() else {
            if !self.revision_seen
                || self.source.counts != DiskANNCanonicalCounts::new(self.current, self.changed)?
            {
                return Err(invalid());
            }
            return Ok(Stage::Changes);
        };
        if tensor.origin.dimensions() != self.source.dimensions
            || tensor.origin.count() != tensor.values.len() as u64
        {
            return Err(invalid());
        }
        self.current = self
            .current
            .checked_add(tensor.origin.count())
            .ok_or(MemoryError::SizeOverflow)?;
        if let Some(version) = self.source.changes.get(&document) {
            if *version != tensor.origin.version() {
                return Err(invalid());
            }
            self.changed = self
                .changed
                .checked_add(tensor.origin.count())
                .ok_or(MemoryError::SizeOverflow)?;
        }
        self.revision_seen |= Some(tensor.origin.version()) == self.source.revision;
        Ok(Stage::Documents)
    }
}
