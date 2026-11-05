//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One cursor composes exact manifest, physical and canonical continuations.

use super::{Canonical, DiskANNMemoryOptions, DiskANNMemorySource, RetainedDiskANNIndex};
use crate::{
    diskann_index::{
        memory::source::{CanonicalImageCursor, CanonicalImageValidation},
        pages::PhysicalImageCursor,
    },
    image_io::{ImagePreparation, ImageRead},
    read_control::StorageReadControl,
    vector_index::image::MAGIC,
    StorageBackendResult,
};
use uqa_core::memory::BudgetedVec;

enum Stage {
    Header,
    Manifest,
    Options,
    Physical,
    Canonical,
    Complete,
}

pub(super) struct DiskANNImageCursor<'a> {
    validation: CanonicalImageValidation<'a>,
    physical: PhysicalImageCursor<'a>,
    canonical: CanonicalImageCursor<'a>,
    manifest: BudgetedVec<u8>,
    header: [u8; 61],
    options: [u8; 40],
    position: usize,
    stage: Stage,
    original: &'a StorageReadControl,
    control: &'a StorageReadControl,
}

impl<'a> DiskANNImageCursor<'a> {
    pub(super) fn new(
        index: &'a RetainedDiskANNIndex<Canonical>,
        physical: &'a DiskANNMemorySource,
        options: DiskANNMemoryOptions,
        original: &'a StorageReadControl,
        control: &'a StorageReadControl,
    ) -> StorageBackendResult<Self> {
        original.check()?;
        control.check()?;
        let manifest = index.manifest();
        let generation = manifest.input().generation;
        let validation = CanonicalImageValidation::new(index.canonical(), physical, generation)?;
        validation.check(control)?;
        let mut header = [0; 61];
        header[..8].copy_from_slice(MAGIC);
        header[8] = 3;
        header[9..13].copy_from_slice(&manifest.input().dimensions.to_le_bytes());
        header[13..29].copy_from_slice(&generation.database());
        for (slot, value) in header[29..53].chunks_exact_mut(8).zip([
            generation.table(),
            generation.index(),
            generation.generation(),
        ]) {
            slot.copy_from_slice(&value.to_le_bytes());
        }
        let manifest = manifest.encode(control)?;
        header[53..].copy_from_slice(&(manifest.len() as u64).to_le_bytes());
        let mut encoded_options = [0; 40];
        for (slot, value) in encoded_options.chunks_exact_mut(8).zip([
            options.read.resident_bytes,
            options.read.cache_bytes,
            options.read.max_in_flight_page_bytes,
            options.read.max_record_bytes,
            options.generation.max_record_bytes,
        ]) {
            slot.copy_from_slice(&(value as u64).to_le_bytes());
        }
        Ok(Self {
            validation,
            physical: PhysicalImageCursor::new(physical),
            canonical: CanonicalImageCursor::new(index.canonical()),
            manifest,
            header,
            options: encoded_options,
            position: 0,
            stage: Stage::Header,
            original,
            control,
        })
    }

    fn check(&self) -> StorageBackendResult<()> {
        self.original.check()?;
        self.validation.check(self.control)
    }

    fn piece(&mut self, output: &mut [u8]) -> StorageBackendResult<usize> {
        let bytes: &[u8] = match self.stage {
            Stage::Header => &self.header,
            Stage::Manifest => &self.manifest,
            Stage::Options => &self.options,
            Stage::Physical => return self.physical.read(output, self.control),
            Stage::Canonical => return self.canonical.read(output, self.control),
            Stage::Complete => return Ok(0),
        };
        let count = (bytes.len() - self.position).min(output.len());
        output[..count].copy_from_slice(&bytes[self.position..self.position + count]);
        self.position += count;
        Ok(count)
    }
}

impl ImageRead for DiskANNImageCursor<'_> {
    fn prepare(&mut self, maximum: usize) -> StorageBackendResult<ImagePreparation> {
        self.check()?;
        let progress = self.validation.advance(maximum, self.control)?;
        self.check()?;
        Ok(progress)
    }

    fn read(&mut self, output: &mut [u8]) -> StorageBackendResult<usize> {
        self.check()?;
        let limit = output.len().min(16 * 1024);
        let mut written = 0;
        while written < limit && !matches!(self.stage, Stage::Complete) {
            self.check()?;
            let count = self.piece(&mut output[written..limit])?;
            written += count;
            if count == 0 {
                self.position = 0;
                self.stage = match self.stage {
                    Stage::Header => Stage::Manifest,
                    Stage::Manifest => Stage::Options,
                    Stage::Options => Stage::Physical,
                    Stage::Physical => Stage::Canonical,
                    Stage::Canonical | Stage::Complete => Stage::Complete,
                };
            }
        }
        self.check()?;
        Ok(written)
    }
}
