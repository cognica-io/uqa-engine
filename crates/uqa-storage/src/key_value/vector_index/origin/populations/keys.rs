//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::diskann_index::format::DiskANNGeneration;
use crate::key_value::{codec, TAG_VECTOR};
use crate::mvcc::{VersionError, VersionResult};
use crate::read_control::StorageReadControl;
use uqa_core::memory::BudgetedVec;

pub(in crate::key_value) const ROOT: &[u8] = b"\0uqa-diskann-populations-v1\0";
pub(super) const HEADER: u8 = b'h';
pub(super) const WITNESS: u8 = b'w';

pub(in crate::key_value) fn field_prefixes(
    table: &str,
    field: &str,
) -> crate::StorageBackendResult<[Vec<u8>; 2]> {
    prefixes(&codec::vector_field_prefix(table, field)?)
}

pub(in crate::key_value) fn table_prefixes(
    table: &str,
) -> crate::StorageBackendResult<[Vec<u8>; 2]> {
    prefixes(&codec::vector_key_prefix(table)?)
}

fn prefixes(field: &[u8]) -> crate::StorageBackendResult<[Vec<u8>; 2]> {
    let prefix = |kind| -> crate::StorageBackendResult<Vec<u8>> {
        let mut output = Vec::new();
        output
            .try_reserve_exact(ROOT.len() + 1 + field.len())
            .map_err(uqa_core::memory::MemoryError::from)?;
        output.extend_from_slice(ROOT);
        output.push(kind);
        output.extend_from_slice(field);
        Ok(output)
    };
    Ok([prefix(HEADER)?, prefix(WITNESS)?])
}

pub(super) fn field_len(field: &[u8]) -> VersionResult<usize> {
    if field.first() != Some(&TAG_VECTOR) {
        return Err(invalid(
            "population field is not a canonical vector address",
        ));
    }
    let mut offset = 1;
    for _ in 0..2 {
        std::str::from_utf8(codec::read_segment(field, &mut offset)?)
            .map_err(|_| invalid("invalid UTF-8 population field"))?;
    }
    Ok(offset)
}

pub(super) fn origin_parts(key: &[u8]) -> VersionResult<(&[u8], u64)> {
    let raw = key
        .strip_prefix(super::super::ROOT)
        .ok_or(invalid("population input is not a canonical origin"))?;
    let end = field_len(raw)?;
    if raw.len() != end + 8 {
        return Err(invalid("invalid canonical origin key width"));
    }
    Ok((
        &raw[..end],
        u64::from_be_bytes(raw[end..].try_into().expect("validated width")),
    ))
}

pub(super) fn parts(key: &[u8], kind: u8) -> VersionResult<(&[u8], DiskANNGeneration)> {
    let body = key
        .strip_prefix(ROOT)
        .ok_or(invalid("invalid population namespace"))?;
    if body.first() != Some(&kind) {
        return Err(invalid("unexpected population record kind"));
    }
    let field = &body[1..];
    let end = field_len(field)?;
    let width = if kind == WITNESS { 48 } else { 40 };
    if field.len() != end + width {
        return Err(invalid("invalid population key width"));
    }
    let identity = &field[end..end + 40];
    let generation = DiskANNGeneration::new(
        identity[..16].try_into().expect("validated width"),
        u64::from_be_bytes(identity[16..24].try_into().expect("validated width")),
        u64::from_be_bytes(identity[24..32].try_into().expect("validated width")),
        u64::from_be_bytes(identity[32..40].try_into().expect("validated width")),
    )?;
    Ok((&field[..end], generation))
}

pub(super) fn header(
    field: &[u8],
    generation: DiskANNGeneration,
    control: &StorageReadControl,
) -> VersionResult<BudgetedVec<u8>> {
    if field_len(field)? != field.len() {
        return Err(invalid("population field address has a trailing suffix"));
    }
    concatenate(
        &[
            ROOT,
            &[HEADER],
            field,
            &generation.database(),
            &generation.table().to_be_bytes(),
            &generation.index().to_be_bytes(),
            &generation.generation().to_be_bytes(),
        ],
        control,
    )
}

pub(super) fn concatenate(
    parts: &[&[u8]],
    control: &StorageReadControl,
) -> VersionResult<BudgetedVec<u8>> {
    let mut output = BudgetedVec::new(control.memory());
    for part in parts {
        for chunk in part.chunks(4096) {
            control.check()?;
            output.extend_from_slice(chunk)?;
        }
    }
    control.check()?;
    Ok(output)
}

pub(super) fn dimensions(bytes: &[u8], offset: usize) -> VersionResult<u32> {
    let width = bytes
        .get(offset..offset + 4)
        .ok_or(invalid("truncated population record"))?;
    Ok(u32::from_le_bytes(width.try_into().expect("checked width")))
}

pub(super) fn invalid(message: &'static str) -> VersionError {
    VersionError::InvalidEncoding(message)
}
