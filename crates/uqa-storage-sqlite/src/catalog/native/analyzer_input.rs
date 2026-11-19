//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native field projections retain the complete object-owned family in one view.

use super::{string_ref, Catalog, Family};
use crate::mvcc::native::{decode_record, NativeRecordIdentity};
use rusqlite::types::ValueRef;
use uqa_core::memory::Budgeted;
use uqa_storage::{
    catalog::{
        analyzer_input::{
            CatalogAnalyzerBindingRow, CatalogAnalyzerInput, CatalogAnalyzerLabelRow,
        },
        input::CatalogInputLimits,
    },
    key_value::KeyValueRead,
    mvcc::VersionError,
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};

impl Catalog {
    pub(in crate::catalog) fn native_analyzer_input(
        &self,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<CatalogAnalyzerInput> {
        control.check()?;
        limits.validate()?;
        let snapshot = self.conn.native_snapshot()?.ok_or_else(|| {
            StorageBackendError::Other("retained analyzer input requires native records".into())
        })?;
        let prefix = NativeRecordIdentity::family_prefix(Family::TableFieldAnalyzers, control)
            .map_err(VersionError::into_storage_error)?;
        let read = snapshot.record_read().retain(&[&prefix])?;
        CatalogAnalyzerInput::new(read, (&prefix, binding), (&prefix, label), limits, control)
    }
}

fn binding(
    key: &[u8],
    value: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Option<Budgeted<CatalogAnalyzerBindingRow>>> {
    let (_, row) = decode_record(key, value, control).map_err(VersionError::into_storage_error)?;
    if row[4] == ValueRef::Null {
        return Ok(None);
    }
    CatalogAnalyzerBindingRow::copy(
        string_ref(row[0])?,
        string_ref(row[1])?,
        string_ref(row[4])?,
        control,
    )
    .map(Some)
}

fn label(
    key: &[u8],
    value: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Option<Budgeted<CatalogAnalyzerLabelRow>>> {
    let (_, row) = decode_record(key, value, control).map_err(VersionError::into_storage_error)?;
    CatalogAnalyzerLabelRow::copy(
        string_ref(row[0])?,
        string_ref(row[1])?,
        string_ref(row[2])?,
        string_ref(row[3])?,
        control,
    )
    .map(Some)
}

#[cfg(test)]
mod tests;
