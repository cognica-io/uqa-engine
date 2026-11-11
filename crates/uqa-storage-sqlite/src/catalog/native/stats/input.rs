//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native statistics bind the table identity and records in the same retained view.

use crate::{
    catalog::{
        native::{string_ref, Family},
        Catalog,
    },
    mvcc::native::{decode_record, NativeRecordIdentity},
    SQLiteError,
};
use rusqlite::types::ValueRef;
use uqa_core::memory::{Budgeted, ProductionControl};
use uqa_storage::{
    catalog::input::{statistics, CatalogInput, CatalogInputLimits},
    key_value::KeyValueRead,
    mvcc::VersionError,
    read_control::StorageReadControl,
    ColumnStatsRow, StorageBackendError, StorageBackendResult,
};

impl Catalog {
    pub(in crate::catalog) fn native_column_stats_input(
        &self,
        table: &str,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<CatalogInput<ColumnStatsRow>> {
        control.check()?;
        let snapshot = self.conn.native_snapshot()?.ok_or_else(|| {
            StorageBackendError::Other("retained catalog input requires native records".into())
        })?;
        let Some(owner) = snapshot.table_owner_controlled(table, control)? else {
            return CatalogInput::empty(limits, control);
        };
        let prefix = NativeRecordIdentity::new(Family::ColumnStats, owner)
            .map_err(VersionError::into_storage_error)?
            .encode_prefix(&[], control)
            .map_err(VersionError::into_storage_error)?;
        let read = snapshot.record_read().retain(&[&prefix])?;
        CatalogInput::new(read, &prefix, decode, limits, control)
    }
}

fn decode(
    key: &[u8],
    value: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Option<Budgeted<ColumnStatsRow>>> {
    let (_, row) = decode_record(key, value, control).map_err(VersionError::into_storage_error)?;
    let production = ProductionControl::new(
        control.memory(),
        control.cancellation(),
        control.cancellation(),
    );
    let text = |index: usize| -> StorageBackendResult<Budgeted<String>> {
        Ok(production
            .copy_text(string_ref(row[index])?)?
            .into_budgeted()
            .expect("catalog input retains an allowance"))
    };
    let integer = |index: usize| -> StorageBackendResult<i64> {
        row[index].as_i64().map_err(|_| {
            SQLiteError::StorageBackend("native statistics count must be integer".into()).into()
        })
    };
    let optional = |index: usize| -> StorageBackendResult<Option<Budgeted<String>>> {
        (row[index] != ValueRef::Null)
            .then(|| text(index))
            .transpose()
    };
    // Preserve the stored row's field-validation order before adopting any buffers.
    let column = text(1)?;
    let distinct = integer(2)?;
    let nulls = integer(3)?;
    let min = optional(4)?;
    let max = optional(5)?;
    let rows = integer(6)?;
    Ok(Some(statistics::row(
        [column, text(7)?, text(8)?, text(9)?],
        [min, max],
        [distinct, nulls, rows],
    )))
}
