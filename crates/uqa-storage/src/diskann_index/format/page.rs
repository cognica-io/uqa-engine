//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use sha2::{Digest, Sha256};
use uqa_core::memory::BudgetedVec;

use super::{
    field, invalid, zeros, DiskANNGeneration, DiskANNNodeLayout, DiskANNPageShape, PAGE_BYTES,
    PAGE_FORMAT_REVISION, PAGE_HEADER_BYTES,
};
use crate::{read_control::StorageReadControl, StorageBackendResult};

const MAGIC: &[u8; 8] = b"UQADNPG\0";
const CHECKSUM_OFFSET: usize = PAGE_HEADER_BYTES - 32;

/// A checked page envelope. Node semantics are validated only after a complete slot is available.
#[derive(Debug)]
pub struct DiskANNPage<'a> {
    id: u64,
    shape: DiskANNPageShape,
    payload: &'a [u8],
}

impl DiskANNPage<'_> {
    pub fn id(&self) -> u64 {
        self.id
    }
    pub fn shape(&self) -> DiskANNPageShape {
        self.shape
    }
    pub fn payload(&self) -> &[u8] {
        self.payload
    }
}

pub fn encode_page(
    generation: DiskANNGeneration,
    layout: DiskANNNodeLayout,
    page_id: u64,
    payload: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<BudgetedVec<u8>> {
    control.check()?;
    let shape = layout.page_shape(page_id)?;
    if payload.len() != shape.payload_bytes as usize {
        return Err(invalid("page payload length differs from its layout"));
    }
    let mut header = [0_u8; PAGE_HEADER_BYTES];
    header[..8].copy_from_slice(MAGIC);
    header[8..12].copy_from_slice(&PAGE_FORMAT_REVISION.to_le_bytes());
    header[16..32].copy_from_slice(&generation.database);
    header[32..40].copy_from_slice(&generation.table.to_le_bytes());
    header[40..48].copy_from_slice(&generation.index.to_le_bytes());
    header[48..56].copy_from_slice(&generation.generation.to_le_bytes());
    header[56..64].copy_from_slice(&page_id.to_le_bytes());
    header[64..72].copy_from_slice(&layout.node_count.to_le_bytes());
    header[72..80].copy_from_slice(&(layout.max_degree as u64).to_le_bytes());
    header[80..84].copy_from_slice(&layout.dimensions.to_le_bytes());
    header[88..96].copy_from_slice(&shape.first_node.to_le_bytes());
    header[96..100].copy_from_slice(&shape.slots.to_le_bytes());
    header[100..104].copy_from_slice(&shape.fragment_index.to_le_bytes());
    header[104..108].copy_from_slice(&shape.fragments.to_le_bytes());
    header[108..112].copy_from_slice(&shape.payload_bytes.to_le_bytes());
    let mut bytes = BudgetedVec::new(control.memory());
    bytes.reserve(PAGE_BYTES)?;
    bytes.extend_from_slice(&header)?;
    bytes.extend_from_slice(payload)?;
    while bytes.len() < PAGE_BYTES {
        crate::diskann_index::metric::checkpoint(bytes.len(), control)?;
        bytes.push(0)?;
    }
    let digest = checksum(&bytes);
    bytes[CHECKSUM_OFFSET..PAGE_HEADER_BYTES].copy_from_slice(&digest);
    control.check()?;
    Ok(bytes)
}

pub fn decode_page<'a>(
    generation: DiskANNGeneration,
    layout: DiskANNNodeLayout,
    expected_page: u64,
    bytes: &'a [u8],
    control: &StorageReadControl,
) -> StorageBackendResult<DiskANNPage<'a>> {
    control.check()?;
    let expected = layout.page_shape(expected_page)?;
    if bytes.len() != PAGE_BYTES
        || &bytes[..8] != MAGIC
        || u32::from_le_bytes(field(bytes, 8)?) != PAGE_FORMAT_REVISION
    {
        return Err(invalid("unrecognized page length, magic or revision"));
    }
    zeros(&bytes[12..16], control)?;
    zeros(&bytes[84..88], control)?;
    let actual = DiskANNGeneration::new(
        field(bytes, 16)?,
        u64::from_le_bytes(field(bytes, 32)?),
        u64::from_le_bytes(field(bytes, 40)?),
        u64::from_le_bytes(field(bytes, 48)?),
    )?;
    if actual != generation || u64::from_le_bytes(field(bytes, 56)?) != expected_page {
        return Err(invalid("page belongs to another generation or address"));
    }
    if u64::from_le_bytes(field(bytes, 64)?) != layout.node_count
        || u64::from_le_bytes(field(bytes, 72)?) != layout.max_degree as u64
        || u32::from_le_bytes(field(bytes, 80)?) != layout.dimensions
    {
        return Err(invalid(
            "page dimensions or graph bounds differ from layout",
        ));
    }
    let shape = DiskANNPageShape {
        first_node: u64::from_le_bytes(field(bytes, 88)?),
        slots: u32::from_le_bytes(field(bytes, 96)?),
        fragment_index: u32::from_le_bytes(field(bytes, 100)?),
        fragments: u32::from_le_bytes(field(bytes, 104)?),
        payload_bytes: u32::from_le_bytes(field(bytes, 108)?),
    };
    if shape != expected {
        return Err(invalid(
            "page slot or fragment metadata differs from layout",
        ));
    }
    if field::<32>(bytes, CHECKSUM_OFFSET)? != checksum(bytes) {
        return Err(invalid("page checksum mismatch"));
    }
    let end = PAGE_HEADER_BYTES + shape.payload_bytes as usize;
    zeros(&bytes[end..], control)?;
    control.check()?;
    Ok(DiskANNPage {
        id: expected_page,
        shape,
        payload: &bytes[PAGE_HEADER_BYTES..end],
    })
}

fn checksum(bytes: &[u8]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(&bytes[..CHECKSUM_OFFSET]);
    hash.update(&bytes[PAGE_HEADER_BYTES..]);
    hash.finalize().into()
}
