//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Incremental BLOB reads release their physical snapshot before returning any chunk.

use super::{
    params, reserve_bindings, runs, CommitSequence, Connection, PhysicalResult, StorageReadControl,
    VersionError,
};
use uqa_storage::mvcc::RecordValueChunk;

pub(super) fn read(
    connection: &Connection,
    key: &[u8],
    boundary: CommitSequence,
    offset: usize,
    output: &mut [u8],
    control: &StorageReadControl,
) -> PhysicalResult<Option<RecordValueChunk>> {
    let _bindings = reserve_bindings(control, &[key])?;
    let Some(info) = super::info(connection, key, boundary)? else {
        control.cancellation().check().map_err(VersionError::from)?;
        return Ok(None);
    };
    if info.run {
        let mut result = None;
        runs::value(connection, key, boundary, control, &mut |record| {
            result = record
                .map(|record| RecordValueChunk::copy_from(record, offset, output, control))
                .transpose()?;
            Ok(())
        })?;
        return Ok(result);
    }
    let result = RecordValueChunk::for_range(
        CommitSequence::from_u64(info.revision),
        info.length,
        offset,
        output.len(),
    )?;
    if let Some(length) = info.length {
        control.cancellation().check().map_err(VersionError::from)?;
        let mut statement = connection.prepare_cached("SELECT v.version_id FROM _uqa_mvcc_version_metadata m JOIN _uqa_mvcc_versions v ON v.version_id = m.version_id AND v.key = m.key AND v.sequence = m.sequence WHERE m.key = ?1 AND m.sequence = ?2 AND typeof(v.value) = 'blob'")?;
        let mut rows = statement.query(params![key, info.revision.to_be_bytes().as_slice()])?;
        let row = rows.next()?.ok_or(VersionError::InvalidEncoding(
            "record chunk payload is missing or invalid",
        ))?;
        let rowid: i64 = row.get(0)?;
        if rowid <= 0 {
            return Err(VersionError::InvalidEncoding(
                "record chunk has invalid physical identity",
            )
            .into());
        }
        let blob = connection.blob_open("main", "_uqa_mvcc_versions", "value", rowid, true)?;
        if blob.len() != length {
            return Err(
                VersionError::InvalidEncoding("record chunk length differs from metadata").into(),
            );
        }
        for (index, part) in output[..result.copied()].chunks_mut(16 * 1024).enumerate() {
            control.cancellation().check().map_err(VersionError::from)?;
            blob.read_at_exact(part, offset + index * 16 * 1024)?;
        }
        blob.close()?;
    }
    control.cancellation().check().map_err(VersionError::from)?;
    Ok(Some(result))
}
