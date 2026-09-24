//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native field identities and vector scalar codecs shared by physical index adapters.

use crate::mvcc::native::{
    decode_record, NativeRecordFamily as Family, NativeRecordIdentity as Identity,
};
use rusqlite::types::ValueRef;
use uqa_core::{memory::BudgetedVec, DocId};
use uqa_storage::{
    mvcc::{VersionError, VersionResult},
    read_control::StorageReadControl,
};

pub(in crate::vector_index) fn invalid() -> VersionError {
    VersionError::InvalidEncoding("invalid native vector record")
}
pub(in crate::vector_index) fn integer(value: ValueRef<'_>) -> VersionResult<i64> {
    value.as_i64().map_err(|_| invalid())
}
pub(in crate::vector_index) fn unsigned(value: i64) -> VersionResult<u64> {
    u64::try_from(value).map_err(|_| invalid())
}
pub(in crate::vector_index) fn size(value: i64) -> VersionResult<usize> {
    usize::try_from(value).map_err(|_| invalid())
}
pub(in crate::vector_index) fn signed(value: u64) -> VersionResult<i64> {
    i64::try_from(value).map_err(|_| invalid())
}
pub(in crate::vector_index) fn ordinal(value: i64) -> VersionResult<u32> {
    u32::try_from(value).map_err(|_| invalid())
}

pub(in crate::vector_index) struct Address {
    pub(in crate::vector_index) identity: Identity,
    pub(in crate::vector_index) field: BudgetedVec<u8>,
    pub(in crate::vector_index) numbers: [i64; 3],
}
impl Address {
    pub(in crate::vector_index) fn decode(
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        let mut field = BudgetedVec::new(control.memory());
        let mut numbers = [0; 3];
        let identity = Identity::visit_key_components(key, control, |position, value| {
            match position {
                0 => field.extend_from_slice(value.as_str().map_err(|_| invalid())?.as_bytes())?,
                1..=3 => numbers[position - 1] = integer(value)?,
                _ => return Err(invalid()),
            }
            Ok(())
        })?;
        Ok(Self {
            identity,
            field,
            numbers,
        })
    }
    pub(in crate::vector_index) fn key(
        &self,
        family: Family,
        numbers: &[i64],
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        let mut components = BudgetedVec::new(control.memory());
        components.push(ValueRef::Text(&self.field))?;
        for number in numbers {
            components.push(ValueRef::Integer(*number))?;
        }
        Identity::new(family, self.identity.owner())?.encode_prefix(&components, control)
    }
}
pub(in crate::vector_index) fn vector(
    value: ValueRef<'_>,
    control: &StorageReadControl,
) -> VersionResult<BudgetedVec<f32>> {
    let bytes = value.as_blob().map_err(|_| invalid())?;
    if !bytes.len().is_multiple_of(4) {
        return Err(invalid());
    }
    let mut output = BudgetedVec::new(control.memory());
    output.reserve(bytes.len() / 4)?;
    for part in bytes.chunks_exact(4) {
        control.cancellation().check()?;
        output.push(f32::from_le_bytes(
            part.try_into().expect("four-byte float"),
        ))?;
    }
    Ok(output)
}

pub(in crate::vector_index) fn vector_id(
    key: &[u8],
    control: &StorageReadControl,
) -> VersionResult<(DocId, u32)> {
    let address = Address::decode(key, control)?;
    if address.identity.family() != Family::Vectors {
        return Err(invalid());
    }
    Ok((unsigned(address.numbers[0])?, ordinal(address.numbers[1])?))
}

pub(in crate::vector_index) fn vector_record(
    key: &[u8],
    value: &[u8],
    control: &StorageReadControl,
) -> VersionResult<(DocId, u32, BudgetedVec<f32>)> {
    let (identity, row) = decode_record(key, value, control)?;
    if identity.family() != Family::Vectors {
        return Err(invalid());
    }
    Ok((
        unsigned(integer(row[2])?)?,
        ordinal(integer(row[3])?)?,
        vector(row[4], control)?,
    ))
}
