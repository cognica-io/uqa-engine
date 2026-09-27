//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native population addressing preserves table ownership and exact field, generation and document identities.

use rusqlite::types::ValueRef;
use uqa_core::memory::BudgetedVec;
use uqa_storage::{
    diskann_index::format::DiskANNGeneration,
    mvcc::{VersionError, VersionResult},
    read_control::StorageReadControl,
};

use super::super::{
    decode_row, encode_row, NativeRecordFamily as Family, NativeRecordIdentity as Identity,
    NativeRecordOwner as Owner,
};

pub(super) fn invalid() -> VersionError {
    VersionError::InvalidEncoding("native DiskANN population identity or payload mismatch")
}

pub(super) fn origin_header_prefix(
    key: &[u8],
    control: &StorageReadControl,
) -> VersionResult<BudgetedVec<u8>> {
    let mut field = BudgetedVec::new(control.memory());
    let identity = Identity::visit_key_components(key, control, |position, value| {
        match position {
            0 => field.extend_from_slice(value.as_str().map_err(|_| invalid())?.as_bytes())?,
            1 if value.as_i64().is_ok_and(|document| document >= 0) => {}
            _ => return Err(invalid()),
        }
        Ok(())
    })?;
    if identity.family() != Family::VectorOrigins {
        return Err(invalid());
    }
    Identity::new(Family::VectorPopulations, identity.owner())?
        .encode_prefix(&[ValueRef::Text(&field)], control)
}

pub(super) fn bytes(value: ValueRef<'_>) -> VersionResult<&[u8]> {
    value.as_blob().map_err(|_| invalid())
}

pub(super) fn dimensions(bytes: &[u8], offset: usize) -> VersionResult<u32> {
    let value = bytes.get(offset..offset + 4).ok_or_else(invalid)?;
    Ok(u32::from_le_bytes(
        value.try_into().expect("validated width"),
    ))
}

pub(super) fn generation(bytes: &[u8]) -> VersionResult<DiskANNGeneration> {
    if bytes.len() != 40 {
        return Err(invalid());
    }
    Ok(DiskANNGeneration::new(
        bytes[..16].try_into().expect("validated width"),
        u64::from_be_bytes(bytes[16..24].try_into().expect("validated width")),
        u64::from_be_bytes(bytes[24..32].try_into().expect("validated width")),
        u64::from_be_bytes(bytes[32..40].try_into().expect("validated width")),
    )?)
}

pub(super) fn generation_bytes(generation: DiskANNGeneration) -> [u8; 40] {
    let mut bytes = [0; 40];
    bytes[..16].copy_from_slice(&generation.database());
    bytes[16..24].copy_from_slice(&generation.table().to_be_bytes());
    bytes[24..32].copy_from_slice(&generation.index().to_be_bytes());
    bytes[32..40].copy_from_slice(&generation.generation().to_be_bytes());
    bytes
}

pub(super) struct Field {
    pub(super) owner: Owner,
    pub(super) table: BudgetedVec<u8>,
    pub(super) name: BudgetedVec<u8>,
}

impl Field {
    pub(super) fn encode(
        owner: Owner,
        table: ValueRef<'_>,
        name: ValueRef<'_>,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        table.as_str().map_err(|_| invalid())?;
        name.as_str().map_err(|_| invalid())?;
        let owner = Identity::new(Family::Vectors, owner)?.encode_prefix(&[], control)?;
        encode_row(&[ValueRef::Blob(&owner), table, name], control)
    }

    pub(super) fn decode(bytes: &[u8], control: &StorageReadControl) -> VersionResult<Self> {
        let row = decode_row(bytes, 3, control)?;
        let owner = Identity::visit_prefix_components(self::bytes(row[0])?, 0, control, |_, _| {
            Err(invalid())
        })?;
        if owner.family() != Family::Vectors {
            return Err(invalid());
        }
        let mut table = BudgetedVec::new(control.memory());
        table.extend_from_slice(row[1].as_str().map_err(|_| invalid())?.as_bytes())?;
        let mut name = BudgetedVec::new(control.memory());
        name.extend_from_slice(row[2].as_str().map_err(|_| invalid())?.as_bytes())?;
        Ok(Self {
            owner: owner.owner(),
            table,
            name,
        })
    }
}

pub(super) struct Address {
    pub(super) owner: Owner,
    pub(super) field: BudgetedVec<u8>,
    pub(super) generation: DiskANNGeneration,
    pub(super) document: Option<u64>,
}

impl Address {
    pub(super) fn decode(key: &[u8], control: &StorageReadControl) -> VersionResult<Self> {
        let mut field = BudgetedVec::new(control.memory());
        let mut generation = None;
        let mut document = None;
        let identity = Identity::visit_key_components(key, control, |position, value| {
            match position {
                0 => field.extend_from_slice(value.as_str().map_err(|_| invalid())?.as_bytes())?,
                1 => generation = Some(self::generation(bytes(value)?)?),
                2 => {
                    document = Some(
                        u64::try_from(value.as_i64().map_err(|_| invalid())?)
                            .map_err(|_| invalid())?,
                    );
                }
                _ => return Err(invalid()),
            }
            Ok(())
        })?;
        if !matches!(
            identity.family(),
            Family::VectorPopulations | Family::VectorPopulationWitnesses
        ) {
            return Err(invalid());
        }
        Ok(Self {
            owner: identity.owner(),
            field,
            generation: generation.ok_or_else(invalid)?,
            document,
        })
    }

    pub(super) fn prefix(
        &self,
        family: Family,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        Identity::new(family, self.owner)?.encode_prefix(
            &[
                ValueRef::Text(&self.field),
                ValueRef::Blob(&generation_bytes(self.generation)),
            ],
            control,
        )
    }
}
