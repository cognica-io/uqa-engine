//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use sha2::{Digest, Sha256};
use uqa_core::memory::BudgetedVec;

use super::{field, invalid, DiskANNGeneration};
use crate::{read_control::StorageReadControl, StorageBackendResult};

pub(super) const HEADER_BYTES: usize = 96;
pub(super) const REVISION: u32 = 1;

pub(super) fn begin(
    magic: [u8; 8],
    generation: DiskANNGeneration,
    body_bytes: usize,
    control: &StorageReadControl,
) -> StorageBackendResult<BudgetedVec<u8>> {
    begin_revision(magic, REVISION, generation, body_bytes, control)
}

pub(super) fn begin_revision(
    magic: [u8; 8],
    revision: u32,
    generation: DiskANNGeneration,
    body_bytes: usize,
    control: &StorageReadControl,
) -> StorageBackendResult<BudgetedVec<u8>> {
    control.check()?;
    let size = HEADER_BYTES
        .checked_add(body_bytes)
        .ok_or_else(|| invalid("record size overflow"))?;
    let mut bytes = BudgetedVec::new(control.memory());
    bytes.reserve(size)?;
    bytes.extend_from_slice(&magic)?;
    bytes.extend_from_slice(&revision.to_le_bytes())?;
    bytes.extend_from_slice(&(HEADER_BYTES as u32).to_le_bytes())?;
    bytes.extend_from_slice(&generation.database)?;
    for value in [
        generation.table,
        generation.index,
        generation.generation,
        body_bytes as u64,
    ] {
        bytes.extend_from_slice(&value.to_le_bytes())?;
    }
    bytes.extend_from_slice(&[0; 32])?;
    Ok(bytes)
}

pub(super) fn finish(
    mut bytes: BudgetedVec<u8>,
    control: &StorageReadControl,
) -> StorageBackendResult<BudgetedVec<u8>> {
    let size = u64::from_le_bytes(field(&bytes, 56)?);
    if bytes.len().checked_sub(HEADER_BYTES).map(|len| len as u64) != Some(size) {
        return Err(invalid("record body length differs"));
    }
    let checksum = checksum(&bytes, control)?;
    bytes[64..96].copy_from_slice(&checksum);
    Ok(bytes)
}

pub(super) async fn open_from<'a>(
    magic: [u8; 8],
    generation: DiskANNGeneration,
    bytes: &'a [u8],
    control: &StorageReadControl,
    work: &mut (impl crate::image_io::work::Work + Send),
) -> StorageBackendResult<&'a [u8]> {
    open_revision_from(magic, REVISION, generation, bytes, control, work).await
}

pub(super) fn open_revision<'a>(
    magic: [u8; 8],
    revision: u32,
    generation: DiskANNGeneration,
    bytes: &'a [u8],
    control: &StorageReadControl,
) -> StorageBackendResult<&'a [u8]> {
    crate::image_io::work::inline(open_revision_from(
        magic,
        revision,
        generation,
        bytes,
        control,
        &mut (),
    ))
}

async fn open_revision_from<'a>(
    magic: [u8; 8],
    revision: u32,
    generation: DiskANNGeneration,
    bytes: &'a [u8],
    control: &StorageReadControl,
    work: &mut (impl crate::image_io::work::Work + Send),
) -> StorageBackendResult<&'a [u8]> {
    control.check()?;
    if bytes.len() < HEADER_BYTES
        || &bytes[..8] != magic.as_slice()
        || u32::from_le_bytes(field(bytes, 8)?) != revision
        || u32::from_le_bytes(field(bytes, 12)?) != HEADER_BYTES as u32
    {
        return Err(invalid(
            "unrecognized record magic, revision or header size",
        ));
    }
    let actual = DiskANNGeneration::new(
        field(bytes, 16)?,
        u64_at(bytes, 32)?,
        u64_at(bytes, 40)?,
        u64_at(bytes, 48)?,
    )?;
    if actual != generation || u64_at(bytes, 56)? != (bytes.len() - HEADER_BYTES) as u64 {
        return Err(invalid("record generation or body length differs"));
    }
    if field::<32>(bytes, 64)? != checksum_from(bytes, control, work).await? {
        return Err(invalid("record checksum mismatch"));
    }
    Ok(&bytes[HEADER_BYTES..])
}

fn checksum(bytes: &[u8], control: &StorageReadControl) -> StorageBackendResult<[u8; 32]> {
    crate::image_io::work::inline(checksum_from(bytes, control, &mut ()))
}

async fn checksum_from(
    bytes: &[u8],
    control: &StorageReadControl,
    work: &mut (impl crate::image_io::work::Work + Send),
) -> StorageBackendResult<[u8; 32]> {
    let mut hash = Sha256::new();
    hash.update(&bytes[..64]);
    update_from(&mut hash, &bytes[HEADER_BYTES..], control, work).await?;
    Ok(hash.finalize().into())
}

pub fn artifact_digest(
    bytes: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<[u8; 32]> {
    crate::image_io::work::inline(artifact_digest_from(bytes, control, &mut ()))
}

pub(in crate::diskann_index) async fn artifact_digest_from(
    bytes: &[u8],
    control: &StorageReadControl,
    work: &mut (impl crate::image_io::work::Work + Send),
) -> StorageBackendResult<[u8; 32]> {
    let mut hash = Sha256::new();
    update_from(&mut hash, bytes, control, work).await?;
    Ok(hash.finalize().into())
}

async fn update_from(
    hash: &mut Sha256,
    bytes: &[u8],
    control: &StorageReadControl,
    work: &mut (impl crate::image_io::work::Work + Send),
) -> StorageBackendResult<()> {
    control.check()?;
    for part in bytes.chunks(4096) {
        work.checkpoint().await?;
        control.check()?;
        hash.update(part);
    }
    control.check()?;
    Ok(())
}

pub(super) fn u32_at(bytes: &[u8], offset: usize) -> StorageBackendResult<u32> {
    Ok(u32::from_le_bytes(field(bytes, offset)?))
}

pub(super) fn u64_at(bytes: &[u8], offset: usize) -> StorageBackendResult<u64> {
    Ok(u64::from_le_bytes(field(bytes, offset)?))
}
