//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use sha2::{Digest, Sha256};
use uqa_core::memory::BudgetedVec;

use super::{
    field, invalid, record, DiskANNBuildCoverage, DiskANNBuildProvenance, DiskANNGeneration,
    DiskANNNodeLayout, DiskANNOriginSummary, NODE_HEADER_BYTES, PAGE_BYTES, PAGE_FORMAT_REVISION,
    PAGE_HEADER_BYTES,
};
use crate::vector_index::{DiskANNAlpha, DiskANNIndexParams};
use crate::{read_control::StorageReadControl, StorageBackendResult};

const MAGIC: [u8; 8] = *b"UQADNMF\0";
const BODY_BYTES: usize = 288;
const NAVIGATION_REVISION: u32 = 1;
const SCORE_REVISION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskANNArtifactDigests {
    pub codebook: [u8; 32],
    pub codes: [u8; 32],
    pub side: [u8; 32],
    pub graph: [u8; 32],
}

impl DiskANNArtifactDigests {
    pub fn empty() -> Self {
        let empty = Sha256::digest([]).into();
        Self {
            codebook: empty,
            codes: empty,
            side: empty,
            graph: empty,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskANNManifestInput {
    pub generation: DiskANNGeneration,
    pub dimensions: u32,
    pub parameters: DiskANNIndexParams,
    pub nodes: u64,
    pub side_vectors: u64,
    pub entry_node: Option<u64>,
    pub coverage: DiskANNBuildCoverage,
    pub artifacts: DiskANNArtifactDigests,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskANNManifest {
    input: DiskANNManifestInput,
    layout: DiskANNNodeLayout,
    provenance: Option<DiskANNBuildProvenance>,
    origins: Option<DiskANNOriginSummary>,
}

impl DiskANNManifest {
    pub const ENCODED_BYTES: usize = record::HEADER_BYTES + BODY_BYTES;
    pub const MAX_ENCODED_BYTES: usize = Self::ENCODED_BYTES
        + DiskANNBuildProvenance::ENCODED_BYTES
        + DiskANNOriginSummary::ENCODED_BYTES;

    pub fn new(input: DiskANNManifestInput) -> StorageBackendResult<Self> {
        input.parameters.validate(input.dimensions)?;
        let layout =
            DiskANNNodeLayout::new(input.dimensions, input.parameters.max_degree, input.nodes)?;
        if input.coverage.generation != input.generation
            || input.coverage.dimensions != input.dimensions
            || input.nodes.checked_add(input.side_vectors) != Some(input.coverage.vectors)
        {
            return Err(invalid(
                "manifest coverage does not match generation and vector counts",
            ));
        }
        match (input.nodes, input.entry_node) {
            (0, None) => {}
            (nodes, Some(entry)) if entry < nodes => {}
            _ => {
                return Err(invalid(
                    "manifest entry must exist exactly for a nonempty graph",
                ))
            }
        }
        let empty = DiskANNArtifactDigests::empty();
        if input.nodes == 0
            && (input.artifacts.codebook != empty.codebook
                || input.artifacts.codes != empty.codes
                || input.artifacts.graph != empty.graph)
            || input.side_vectors == 0 && input.artifacts.side != empty.side
        {
            return Err(invalid("empty artifacts require the empty digest"));
        }
        Ok(Self {
            input,
            layout,
            provenance: None,
            origins: None,
        })
    }

    /// Attach validated construction metadata using manifest envelope revision 2. Node/page/index layout revisions remain unchanged.
    pub fn with_build_provenance(
        mut self,
        provenance: DiskANNBuildProvenance,
    ) -> StorageBackendResult<Self> {
        provenance.validate(self.input.nodes, self.input.parameters)?;
        self.provenance = Some(provenance);
        Ok(self)
    }

    pub fn build_provenance(&self) -> Option<&DiskANNBuildProvenance> {
        self.provenance.as_ref()
    }

    /// Revision 3 carries the complete document-origin artifact, including empty tensors. Its digest does not establish source or publication authority.
    pub(crate) fn with_origins(
        mut self,
        origins: DiskANNOriginSummary,
    ) -> StorageBackendResult<Self> {
        if self.provenance.is_none()
            || (origins.documents() == 0 && self.input.coverage.vectors != 0)
        {
            return Err(invalid(
                "origin metadata requires build provenance and complete documents",
            ));
        }
        self.origins = Some(origins);
        Ok(self)
    }

    pub fn origins(&self) -> Option<DiskANNOriginSummary> {
        self.origins
    }

    pub fn input(&self) -> &DiskANNManifestInput {
        &self.input
    }
    pub fn layout(self) -> DiskANNNodeLayout {
        self.layout
    }

    pub fn encode(self, control: &StorageReadControl) -> StorageBackendResult<BudgetedVec<u8>> {
        let input = self.input;
        let params = input.parameters;
        let (revision, size) = if self.origins.is_some() {
            (
                3,
                BODY_BYTES
                    + DiskANNBuildProvenance::ENCODED_BYTES
                    + DiskANNOriginSummary::ENCODED_BYTES,
            )
        } else if self.provenance.is_some() {
            (2, BODY_BYTES + DiskANNBuildProvenance::ENCODED_BYTES)
        } else {
            (1, BODY_BYTES)
        };
        let mut bytes = record::begin_revision(MAGIC, revision, input.generation, size, control)?;
        for value in [
            input.dimensions,
            params.algorithm_revision,
            NAVIGATION_REVISION,
            SCORE_REVISION,
            PAGE_FORMAT_REVISION,
            PAGE_BYTES as u32,
            NODE_HEADER_BYTES as u32,
            PAGE_HEADER_BYTES as u32,
        ] {
            bytes.extend_from_slice(&value.to_le_bytes())?;
        }
        for value in [
            params.max_degree as u64,
            params.build_list_size as u64,
            params.search_list_size as u64,
            params.alpha.get().to_bits(),
            params.beam_width as u64,
            params.pq_bytes as u64,
            params.seed,
            input.nodes,
            input.side_vectors,
            input.entry_node.unwrap_or(u64::MAX),
        ] {
            bytes.extend_from_slice(&value.to_le_bytes())?;
        }
        bytes.extend_from_slice(&input.coverage.digest)?;
        bytes.extend_from_slice(&input.coverage.vectors.to_le_bytes())?;
        bytes.extend_from_slice(&self.layout.page_count().to_le_bytes())?;
        for digest in [
            input.artifacts.codebook,
            input.artifacts.codes,
            input.artifacts.side,
            input.artifacts.graph,
        ] {
            bytes.extend_from_slice(&digest)?;
        }
        if let Some(provenance) = self.provenance {
            provenance.encode(&mut bytes)?;
        }
        if let Some(origins) = self.origins {
            bytes.extend_from_slice(&origins.documents().to_le_bytes())?;
            bytes.extend_from_slice(&origins.digest())?;
        }
        record::finish(bytes, control)
    }

    pub fn decode(
        generation: DiskANNGeneration,
        bytes: &[u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let revision = record::u32_at(bytes, 8)?;
        let size = match revision {
            1 => BODY_BYTES,
            2 => BODY_BYTES + DiskANNBuildProvenance::ENCODED_BYTES,
            3 => {
                BODY_BYTES
                    + DiskANNBuildProvenance::ENCODED_BYTES
                    + DiskANNOriginSummary::ENCODED_BYTES
            }
            _ => return Err(invalid("unsupported manifest envelope revision")),
        };
        let body = record::open_revision(MAGIC, revision, generation, bytes, control)?;
        if body.len() != size {
            return Err(invalid("manifest body size differs"));
        }
        for (offset, expected) in [
            (8, NAVIGATION_REVISION),
            (12, SCORE_REVISION),
            (16, PAGE_FORMAT_REVISION),
            (20, PAGE_BYTES as u32),
            (24, NODE_HEADER_BYTES as u32),
            (28, PAGE_HEADER_BYTES as u32),
        ] {
            if record::u32_at(body, offset)? != expected {
                return Err(invalid("manifest metric or page layout revision differs"));
            }
        }
        let dimensions = record::u32_at(body, 0)?;
        let size = |offset| {
            usize::try_from(record::u64_at(body, offset)?)
                .map_err(|_| invalid("manifest size exceeds platform"))
        };
        let parameters = DiskANNIndexParams {
            max_degree: size(32)?,
            build_list_size: size(40)?,
            search_list_size: size(48)?,
            alpha: DiskANNAlpha::new(f64::from_bits(record::u64_at(body, 56)?))?,
            beam_width: size(64)?,
            pq_bytes: size(72)?,
            seed: record::u64_at(body, 80)?,
            format_revision: DiskANNIndexParams::FORMAT_REVISION,
            algorithm_revision: record::u32_at(body, 4)?,
        };
        let entry = record::u64_at(body, 104)?;
        let mut manifest = Self::new(DiskANNManifestInput {
            generation,
            dimensions,
            parameters,
            nodes: record::u64_at(body, 88)?,
            side_vectors: record::u64_at(body, 96)?,
            entry_node: (entry != u64::MAX).then_some(entry),
            coverage: DiskANNBuildCoverage {
                generation,
                dimensions,
                vectors: record::u64_at(body, 144)?,
                digest: field(body, 112)?,
            },
            artifacts: DiskANNArtifactDigests {
                codebook: field(body, 160)?,
                codes: field(body, 192)?,
                side: field(body, 224)?,
                graph: field(body, 256)?,
            },
        })?;
        if record::u64_at(body, 152)? != manifest.layout.page_count() {
            return Err(invalid("manifest page count differs from node layout"));
        }
        if revision >= 2 {
            manifest = manifest.with_build_provenance(DiskANNBuildProvenance::decode(
                &body[BODY_BYTES..BODY_BYTES + DiskANNBuildProvenance::ENCODED_BYTES],
                manifest.input.nodes,
                parameters,
            )?)?;
        }
        if revision == 3 {
            let offset = BODY_BYTES + DiskANNBuildProvenance::ENCODED_BYTES;
            manifest = manifest.with_origins(DiskANNOriginSummary::new(
                record::u64_at(body, offset)?,
                field(body, offset + 8)?,
            )?)?;
        }
        Ok(manifest)
    }
}
