//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use sha2::{Digest, Sha256};
use uqa_core::DocId;

use super::{invalid, DiskANNGeneration, DiskANNVectorVersion};
use crate::diskann_index::metric::checkpoint;
use crate::vector_index::validate_vector_values_controlled;
use crate::{read_control::StorageReadControl, StorageBackendResult};

/// A fingerprint of selected canonical versions, not a visibility watermark or membership oracle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskANNBuildCoverage {
    pub(super) generation: DiskANNGeneration,
    pub(super) dimensions: u32,
    pub(super) vectors: u64,
    pub(super) digest: [u8; 32],
}

impl DiskANNBuildCoverage {
    pub fn vector_count(self) -> u64 {
        self.vectors
    }
    pub fn digest(self) -> [u8; 32] {
        self.digest
    }
}

pub struct DiskANNCoverageBuilder {
    generation: DiskANNGeneration,
    dimensions: u32,
    vectors: u64,
    previous: Option<(DocId, u32)>,
    hash: Sha256,
}

impl DiskANNCoverageBuilder {
    pub fn new(generation: DiskANNGeneration, dimensions: u32) -> StorageBackendResult<Self> {
        if dimensions == 0 {
            return Err(invalid("coverage dimensions must be positive"));
        }
        let mut hash = Sha256::new();
        hash.update(b"UQA DiskANN canonical coverage\0\x01");
        hash.update(generation.database);
        for value in [generation.table, generation.index, generation.generation] {
            hash.update(value.to_le_bytes());
        }
        hash.update(dimensions.to_le_bytes());
        Ok(Self {
            generation,
            dimensions,
            vectors: 0,
            previous: None,
            hash,
        })
    }

    /// Supply every visible ordinal in canonical document order. Failure leaves the accepted prefix unchanged.
    pub fn push(
        &mut self,
        doc: DocId,
        ordinal: u32,
        version: DiskANNVectorVersion,
        raw: &[f32],
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        control.check()?;
        let ordered = match self.previous {
            None => ordinal == 0,
            Some((previous, last)) if previous == doc => last.checked_add(1) == Some(ordinal),
            Some((previous, _)) => doc > previous && ordinal == 0,
        };
        if !ordered {
            return Err(invalid(
                "coverage input must contain ordered complete tensor ordinals",
            ));
        }
        validate_vector_values_controlled(self.dimensions, raw, Some(control))?;
        let vectors = self
            .vectors
            .checked_add(1)
            .ok_or_else(|| invalid("coverage count overflow"))?;
        let mut hash = self.hash.clone();
        hash.update(doc.to_le_bytes());
        hash.update(ordinal.to_le_bytes());
        hash.update(version.writer.database().as_bytes());
        hash.update(version.writer.allocation().to_le_bytes());
        hash.update(version.revision.to_le_bytes());
        for (offset, value) in raw.iter().enumerate() {
            checkpoint(offset, control)?;
            hash.update(value.to_bits().to_le_bytes());
        }
        control.check()?;
        self.hash = hash;
        self.vectors = vectors;
        self.previous = Some((doc, ordinal));
        Ok(())
    }

    pub fn finish(mut self) -> DiskANNBuildCoverage {
        self.hash.update(self.vectors.to_le_bytes());
        DiskANNBuildCoverage {
            generation: self.generation,
            dimensions: self.dimensions,
            vectors: self.vectors,
            digest: self.hash.finalize().into(),
        }
    }
}
