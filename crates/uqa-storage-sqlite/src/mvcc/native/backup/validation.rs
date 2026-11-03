//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical image dependencies use the existing native and generation owners.

use super::super::{invalid, NativeRecordFamily as Family};
use crate::mvcc::{Error, PhysicalResult};
use rusqlite::{params, types::ValueRef, Connection};
use uqa_storage::{
    key_value::diskann_backup_membership, mvcc::VersionError, read_control::StorageReadControl,
};

pub(in crate::mvcc::native) fn validate_diskann(
    connection: &Connection,
    family: Family,
    values: &[ValueRef<'_>],
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    if family != Family::DiskANNRecords {
        return Ok(());
    }
    let ValueRef::Blob(key) = values[0] else {
        return Err(invalid("native image DiskANN key type").into());
    };
    let (included, _) = diskann_backup_membership(key, |key, output| {
        (|| -> PhysicalResult<usize> {
            let _bindings = crate::read_control::reserve_bindings(control, &[key])?;
            let mut statement = connection.prepare_cached("SELECT length(value), substr(value, 1, ?2) FROM _uqa_mvcc_native_diskann_records WHERE key = ?1")?;
            let expected = i64::try_from(output.len()).map_err(|_| invalid("native image state buffer length"))?;
            let mut rows = statement.query(params![key, expected])?;
            let row = rows.next()?.ok_or_else(|| invalid("native image DiskANN state missing"))?;
            let length: i64 = row.get(0)?;
            let value = row.get_ref(1)?.as_blob().map_err(|_| invalid("native image DiskANN state type"))?;
            if length != expected || value.len() != output.len() { return Err(invalid("native image DiskANN state length").into()); }
            output.copy_from_slice(value);
            Ok(output.len())
        })().map_err(|error| Error::into_version(error).into_storage_error())
    }).map_err(VersionError::Storage)?;
    if !included {
        return Err(invalid("native image contains an unpublished DiskANN generation").into());
    }
    Ok(())
}
