//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native metadata rows adapt the shared statistics-maintenance merge contract.

use rusqlite::types::ValueRef;
use uqa_core::memory::BudgetedVec;
use uqa_storage::{
    mvcc::{MaintenanceRecordLayout, VersionError, VersionResult},
    read_control::StorageReadControl,
    statistics_maintenance::StatisticsMaintenance,
};

use super::{decode_record, encode_row, NativeRecordFamily};

pub(in crate::mvcc) struct NativeMaintenanceRecords;

fn row<'a>(
    key: &[u8],
    value: &'a [u8],
    control: &StorageReadControl,
) -> VersionResult<BudgetedVec<ValueRef<'a>>> {
    let (identity, values) = decode_record(key, value, control)?;
    if identity.family() != NativeRecordFamily::Metadata
        || !values[0]
            .as_str()
            .ok()
            .is_some_and(|name| name.starts_with(StatisticsMaintenance::KEY_PREFIX))
    {
        return Err(VersionError::InvalidEncoding(
            "invalid native statistics-maintenance key",
        ));
    }
    Ok(values)
}

impl MaintenanceRecordLayout for NativeMaintenanceRecords {
    fn decode(
        &self,
        key: &[u8],
        value: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<StatisticsMaintenance> {
        let values = row(key, value, control)?;
        let text = values[1].as_str().map_err(|_| {
            VersionError::InvalidEncoding("invalid statistics-maintenance JSON storage class")
        })?;
        serde_json::from_str(text)
            .map_err(uqa_storage::StorageBackendError::from)
            .map_err(Into::into)
    }

    fn encode(
        &self,
        key: &[u8],
        template: &[u8],
        state: &StatisticsMaintenance,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        let values = row(key, template, control)?;
        let json = state.encode(control)?;
        encode_row(&[values[0], ValueRef::Text(&json)], control)
    }
}
