//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native definition input preserves borrowed row decoding and admitted field construction.

use super::{string_ref, Catalog, Family, NativeRecordOwner};
use crate::mvcc::native::{decode_record, NativeRecordIdentity};
use rusqlite::types::ValueRef;
use uqa_core::{
    memory::{Budgeted, ProductionControl},
    RelationName, RelationReference,
};
use uqa_storage::{
    catalog::input::{CatalogInput, CatalogInputLimits, CatalogRowDecoder},
    key_value::KeyValueRead,
    read_control::StorageReadControl,
    CatalogIndexRow, StorageBackendError, StorageBackendResult,
};

impl Catalog {
    pub(in crate::catalog) fn native_row_input<T>(
        &self,
        family: Family,
        decode: CatalogRowDecoder<T>,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<CatalogInput<T>> {
        control.check()?;
        let snapshot = self.conn.native_snapshot()?.ok_or_else(|| {
            StorageBackendError::Other("retained catalog input requires native records".into())
        })?;
        let prefix =
            NativeRecordIdentity::new(family, NativeRecordOwner::Database(snapshot.database))
                .map_err(uqa_storage::mvcc::VersionError::into_storage_error)?
                .encode_prefix(&[], control)
                .map_err(uqa_storage::mvcc::VersionError::into_storage_error)?;
        let read = snapshot.record_read().retain(&[&prefix])?;
        CatalogInput::new(read, &prefix, decode, limits, control)
    }

    pub(in crate::catalog) fn native_index_input(
        &self,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<CatalogInput<CatalogIndexRow>> {
        self.native_row_input(Family::CatalogIndexes, decode_index, limits, control)
    }
}

fn decode_index(
    key: &[u8],
    value: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Option<Budgeted<CatalogIndexRow>>> {
    let (_, row) = decode_record(key, value, control)
        .map_err(uqa_storage::mvcc::VersionError::into_storage_error)?;
    let production = ProductionControl::new(
        control.memory(),
        control.cancellation(),
        control.cancellation(),
    );
    let reference = |schema, name| -> StorageBackendResult<RelationReference<'_>> {
        Ok(RelationReference {
            schema: Some(RelationName::decoded(string_ref(row[schema])?)),
            name: RelationName::decoded(string_ref(row[name])?),
        })
    };
    let relation = reference(0, 1)?
        .identity_with_control(RelationName::decoded("public"), &production)?
        .into_budgeted()
        .expect("catalog input retains an allowance");
    let table = reference(4, 5)?
        .render_with_control(&production)?
        .into_budgeted()
        .expect("catalog input retains an allowance");
    let text = |column| -> StorageBackendResult<Budgeted<String>> {
        Ok(production
            .copy_text(string_ref(row[column])?)?
            .into_budgeted()
            .expect("catalog input retains an allowance"))
    };
    Ok(Some(uqa_storage::catalog::input::index::row(
        relation,
        [text(3)?, table, text(6)?, text(7)?],
        (row[8] != ValueRef::Null).then(|| text(8)).transpose()?,
    )))
}
