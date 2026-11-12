//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native sequence definitions and generation values retain one selected record owner.

mod row;
use super::{Catalog, Family, NativeRecordOwner};
use crate::mvcc::native::{decode_record, NativeRecordIdentity};
use uqa_core::memory::Budgeted;
use uqa_storage::{
    catalog::{
        input::CatalogInputLimits,
        sequence_load::{CatalogSequenceDefinition, CatalogSequenceLoad},
        SequenceValuePosition,
    },
    key_value::KeyValueRead,
    mvcc::VersionError,
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};

impl Catalog {
    pub(in crate::catalog) fn native_sequence_load(
        &self,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<CatalogSequenceLoad> {
        control.check()?;
        limits.validate()?;
        let snapshot = self.conn.native_snapshot()?.ok_or_else(|| {
            StorageBackendError::Other("retained sequence input requires native records".into())
        })?;
        let definitions = NativeRecordIdentity::family_prefix(Family::Sequences, control)
            .map_err(VersionError::into_storage_error)?;
        let values = NativeRecordIdentity::family_prefix(Family::SequenceValues, control)
            .map_err(VersionError::into_storage_error)?;
        let read = snapshot.record_read().retain(&[&definitions, &values])?;
        CatalogSequenceLoad::new(read, &definitions, decode, position, limits, control)
    }
}

fn decode(
    key: &[u8],
    bytes: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Option<Budgeted<CatalogSequenceDefinition>>> {
    let (_, fields) =
        decode_record(key, bytes, control).map_err(VersionError::into_storage_error)?;
    row::read(&fields, control)?
        .try_map(|row| {
            Ok(CatalogSequenceDefinition {
                row,
                legacy_position: None,
            })
        })
        .map(Some)
}

fn position(
    read: &dyn KeyValueRead,
    definition: &CatalogSequenceDefinition,
    limits: CatalogInputLimits,
    control: &StorageReadControl,
) -> StorageBackendResult<SequenceValuePosition> {
    let row = &definition.row;
    let key = NativeRecordIdentity::new(
        Family::SequenceValues,
        NativeRecordOwner::Object {
            identity: row.object_id,
            generation: row.definition_generation,
        },
    )
    .and_then(|id| id.encode_key(&[], control))
    .map_err(VersionError::into_storage_error)?;
    control.check_value_size(key.len(), limits.maximum_key_bytes)?;
    let mut position = None;
    read.visit_value_bounded(&key, limits.maximum_value_bytes, control, &mut |bytes| {
        if let Some(bytes) = bytes {
            let (_, row) =
                decode_record(&key, bytes, control).map_err(VersionError::into_storage_error)?;
            position = Some(super::sequences::value_position(&row));
        }
        Ok(())
    })?;
    control.check()?;
    read.control().check()?;
    position.ok_or_else(|| {
        StorageBackendError::Other(format!(
            "sequence `{}` has no value record",
            row.relation.qualified_name()
        ))
    })
}
