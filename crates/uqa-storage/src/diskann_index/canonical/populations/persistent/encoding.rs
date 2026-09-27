//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    invalid, DiskANNCanonicalCounts, DiskANNCanonicalOrigin, DiskANNGeneration,
    DiskANNPopulationState, DiskANNPopulationWitness, DocId, StorageBackendResult,
};

impl DiskANNPopulationState {
    pub const ENCODED_BYTES: usize = 72;

    pub fn encode(self) -> [u8; Self::ENCODED_BYTES] {
        let mut bytes = [0; Self::ENCODED_BYTES];
        bytes[..8].copy_from_slice(b"UQAVPOP1");
        bytes[8..48].copy_from_slice(&generation_bytes(self.generation));
        bytes[48..52].copy_from_slice(&self.dimensions.to_le_bytes());
        bytes[56..64].copy_from_slice(&self.counts.current_vectors().to_le_bytes());
        bytes[64..72].copy_from_slice(&self.counts.changed_vectors().to_le_bytes());
        bytes
    }

    /// Decode a fixed-size record only for its independently selected field and generation. Missing records are handled by the lifecycle owner, never fabricated as zero counts.
    pub fn decode(
        bytes: &[u8],
        generation: DiskANNGeneration,
        dimensions: u32,
    ) -> StorageBackendResult<Self> {
        if bytes.len() != Self::ENCODED_BYTES
            || &bytes[..8] != b"UQAVPOP1"
            || bytes[8..48] != generation_bytes(generation)
            || dimensions == 0
            || bytes[48..52] != dimensions.to_le_bytes()
            || bytes[52..56] != [0; 4]
        {
            return Err(invalid("invalid canonical population envelope"));
        }
        Ok(Self {
            generation,
            dimensions,
            counts: DiskANNCanonicalCounts::new(read_u64(bytes, 56), read_u64(bytes, 64))?,
        })
    }
}

impl DiskANNPopulationWitness {
    pub const ENCODED_BYTES: usize = 120;

    pub fn encode(self) -> [u8; Self::ENCODED_BYTES] {
        let mut bytes = [0; Self::ENCODED_BYTES];
        bytes[..8].copy_from_slice(b"UQAVCOV1");
        bytes[8..48].copy_from_slice(&generation_bytes(self.generation));
        bytes[48..56].copy_from_slice(&self.document.to_le_bytes());
        bytes[56..112].copy_from_slice(&self.origin.encode());
        bytes[112] = u8::from(self.covered);
        bytes
    }

    /// Require the actual preceding canonical origin, not just its writer revision. This rejects a stale count/shape, another document's witness and another selected generation before population arithmetic.
    pub fn decode(
        bytes: &[u8],
        generation: DiskANNGeneration,
        document: DocId,
        origin: DiskANNCanonicalOrigin,
    ) -> StorageBackendResult<Self> {
        if bytes.len() != Self::ENCODED_BYTES
            || &bytes[..8] != b"UQAVCOV1"
            || bytes[8..48] != generation_bytes(generation)
            || bytes[48..56] != document.to_le_bytes()
            || bytes[56..112] != origin.encode()
            || bytes[112] > 1
            || bytes[113..] != [0; 7]
        {
            return Err(invalid("invalid canonical population witness envelope"));
        }
        Ok(Self {
            generation,
            document,
            origin,
            covered: bytes[112] != 0,
        })
    }
}

fn generation_bytes(generation: DiskANNGeneration) -> [u8; 40] {
    let mut bytes = [0; 40];
    bytes[..16].copy_from_slice(&generation.database());
    bytes[16..24].copy_from_slice(&generation.table().to_le_bytes());
    bytes[24..32].copy_from_slice(&generation.index().to_le_bytes());
    bytes[32..40].copy_from_slice(&generation.generation().to_le_bytes());
    bytes
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(
        bytes[offset..offset + 8]
            .try_into()
            .expect("validated width"),
    )
}
