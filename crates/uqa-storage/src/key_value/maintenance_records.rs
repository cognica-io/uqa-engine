//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Key/Value encoding for typed statistics-maintenance records.

use crate::{
    mvcc::{MaintenanceRecordLayout, VersionError, VersionResult},
    read_control::StorageReadControl,
    statistics_maintenance::StatisticsMaintenance,
};
use uqa_core::memory::BudgetedVec;

pub struct KeyValueMaintenanceRecords;

fn validate_key(key: &[u8]) -> VersionResult<()> {
    let length = key
        .get(1..5)
        .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
        .map(u32::from_be_bytes)
        .and_then(|length| usize::try_from(length).ok());
    if key.first() != Some(&super::TAG_METADATA)
        || length != key.len().checked_sub(5)
        || !key
            .get(5..)
            .is_some_and(|name| name.starts_with(StatisticsMaintenance::KEY_PREFIX.as_bytes()))
    {
        return Err(VersionError::InvalidEncoding(
            "invalid statistics-maintenance key",
        ));
    }
    Ok(())
}

impl MaintenanceRecordLayout for KeyValueMaintenanceRecords {
    fn decode(
        &self,
        key: &[u8],
        value: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<StatisticsMaintenance> {
        control.cancellation().check()?;
        validate_key(key)?;
        serde_json::from_slice(value)
            .map_err(crate::StorageBackendError::from)
            .map_err(Into::into)
    }

    fn encode(
        &self,
        key: &[u8],
        _template: &[u8],
        state: &StatisticsMaintenance,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        validate_key(key)?;
        state.encode(control)
    }
}
