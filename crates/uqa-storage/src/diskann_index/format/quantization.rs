//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::memory::BudgetedVec;

use super::{field, invalid, record, zeros, DiskANNGeneration, DiskANNManifest};
use crate::diskann_index::{
    metric::checkpoint,
    pq::{PQCodebook, PQTrainingOptions, PQTrainingSummary},
};
use crate::{read_control::StorageReadControl, StorageBackendResult};

const MAGIC: [u8; 8] = *b"UQADNPQ\0";
const METADATA_BYTES: usize = 64;
const SCALAR_REVISION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskANNQuantizationIdentity {
    pub(super) generation: DiskANNGeneration,
    pub(super) dimensions: u32,
    pub(super) chunks: usize,
    pub(super) centroids: u16,
    pub(super) nodes: u64,
    pub(super) digest: [u8; 32],
}

impl DiskANNQuantizationIdentity {
    pub fn codebook_digest(self) -> [u8; 32] {
        self.digest
    }
    pub fn node_count(self) -> u64 {
        self.nodes
    }
    pub fn pq_bytes(self) -> usize {
        self.chunks
    }

    fn new(generation: DiskANNGeneration, codebook: &PQCodebook, digest: [u8; 32]) -> Self {
        Self {
            generation,
            dimensions: codebook.dimensions() as u32,
            chunks: codebook.pq_bytes(),
            centroids: codebook.centroid_count(),
            nodes: codebook.training().observed_vectors,
            digest,
        }
    }
}

fn size(chunks: usize, scalars: usize) -> StorageBackendResult<usize> {
    chunks
        .checked_add(1)
        .and_then(|count| count.checked_mul(4))
        .and_then(|offsets| {
            scalars
                .checked_mul(8)
                .and_then(|coordinates| offsets.checked_add(coordinates))
        })
        .and_then(|body| body.checked_add(METADATA_BYTES))
        .ok_or_else(|| invalid("codebook body size overflow"))
}

pub fn encode_codebook(
    generation: DiskANNGeneration,
    codebook: &PQCodebook,
    control: &StorageReadControl,
) -> StorageBackendResult<(BudgetedVec<u8>, DiskANNQuantizationIdentity)> {
    let summary = codebook.training();
    let options = summary.options;
    let dimensions =
        u32::try_from(codebook.dimensions()).map_err(|_| invalid("codebook dimension range"))?;
    let chunks = codebook.pq_bytes();
    let count = codebook.centroid_count();
    let scalars = summary.validate(dimensions, chunks, count)?;
    let mut bytes = record::begin(MAGIC, generation, size(chunks, scalars)?, control)?;
    bytes.extend_from_slice(&dimensions.to_le_bytes())?;
    bytes.extend_from_slice(&(chunks as u32).to_le_bytes())?;
    bytes.extend_from_slice(&count.to_le_bytes())?;
    bytes.extend_from_slice(&[0; 2])?;
    for value in [
        SCALAR_REVISION,
        PQCodebook::CODEC_REVISION,
        PQCodebook::TRAINING_REVISION,
        options.max_samples,
        options.max_iterations,
    ] {
        bytes.extend_from_slice(&value.to_le_bytes())?;
    }
    bytes.extend_from_slice(&options.max_centroids.to_le_bytes())?;
    bytes.extend_from_slice(&[0; 2])?;
    bytes.extend_from_slice(&summary.sampled_vectors.to_le_bytes())?;
    for value in [options.seed, summary.observed_vectors, scalars as u64] {
        bytes.extend_from_slice(&value.to_le_bytes())?;
    }
    for chunk in 0..chunks {
        checkpoint(chunk, control)?;
        let start = codebook.chunk_range(chunk).expect("validated chunk").start;
        bytes.extend_from_slice(&(start as u32).to_le_bytes())?;
    }
    bytes.extend_from_slice(&dimensions.to_le_bytes())?;
    for chunk in 0..chunks {
        for (offset, coordinate) in codebook
            .chunk_centroids(chunk)
            .expect("validated chunk")
            .iter()
            .enumerate()
        {
            checkpoint(offset, control)?;
            bytes.extend_from_slice(&coordinate.to_bits().to_le_bytes())?;
        }
    }
    let bytes = record::finish(bytes, control)?;
    let digest = record::artifact_digest(&bytes, control)?;
    Ok((
        bytes,
        DiskANNQuantizationIdentity::new(generation, codebook, digest),
    ))
}

pub fn decode_codebook(
    manifest: &DiskANNManifest,
    bytes: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<(PQCodebook, DiskANNQuantizationIdentity)> {
    let input = manifest.input();
    let body = record::open(MAGIC, input.generation, bytes, control)?;
    if input.nodes == 0
        || body.len() < METADATA_BYTES
        || record::artifact_digest(bytes, control)? != input.artifacts.codebook
    {
        return Err(invalid(
            "codebook is absent or differs from manifest digest",
        ));
    }
    let dimensions = record::u32_at(body, 0)?;
    let chunks = record::u32_at(body, 4)? as usize;
    let count = u16::from_le_bytes(field(body, 8)?);
    zeros(&body[10..12], control)?;
    zeros(&body[34..36], control)?;
    for (offset, expected) in [
        (12, SCALAR_REVISION),
        (16, PQCodebook::CODEC_REVISION),
        (20, PQCodebook::TRAINING_REVISION),
    ] {
        if record::u32_at(body, offset)? != expected {
            return Err(invalid(
                "unrecognized codebook scalar, codec or training revision",
            ));
        }
    }
    let summary = PQTrainingSummary {
        options: PQTrainingOptions {
            max_samples: record::u32_at(body, 24)?,
            max_iterations: record::u32_at(body, 28)?,
            max_centroids: u16::from_le_bytes(field(body, 32)?),
            seed: record::u64_at(body, 40)?,
        },
        sampled_vectors: record::u32_at(body, 36)?,
        observed_vectors: record::u64_at(body, 48)?,
    };
    if dimensions != input.dimensions
        || chunks != input.parameters.pq_bytes
        || summary.observed_vectors != input.nodes
        || summary.options.seed != input.parameters.seed
        || manifest
            .build_provenance()
            .is_some_and(|build| build.training() != summary.options)
    {
        return Err(invalid(
            "codebook shape, corpus or seed differs from manifest",
        ));
    }
    let scalars = summary.validate(dimensions, chunks, count)?;
    if record::u64_at(body, 56)? != scalars as u64 || body.len() != size(chunks, scalars)? {
        return Err(invalid("codebook scalar count or length differs"));
    }
    for chunk in 0..=chunks {
        checkpoint(chunk, control)?;
        let expected = if chunk == chunks {
            dimensions as usize
        } else {
            crate::diskann_index::pq::chunk_range(dimensions as usize, chunks, chunk).start
        };
        if record::u32_at(body, METADATA_BYTES + chunk * 4)? as usize != expected {
            return Err(invalid("codebook chunk offsets differ"));
        }
    }
    let mut centroids = BudgetedVec::new(control.memory());
    centroids.reserve(scalars)?;
    let start = METADATA_BYTES + (chunks + 1) * 4;
    for (offset, scalar) in body[start..].chunks_exact(8).enumerate() {
        checkpoint(offset, control)?;
        centroids.push(f64::from_bits(u64::from_le_bytes(field(scalar, 0)?)))?;
    }
    let codebook = PQCodebook::restore(dimensions, chunks, count, summary, centroids, control)?;
    let identity =
        DiskANNQuantizationIdentity::new(input.generation, &codebook, input.artifacts.codebook);
    Ok((codebook, identity))
}
