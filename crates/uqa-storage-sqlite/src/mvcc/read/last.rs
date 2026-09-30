//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Descending seeks merge point and compacted-run identities on the pinned boundary.

use rusqlite::{params, Connection, OptionalExtension};
use uqa_core::memory::BudgetedVec;
use uqa_storage::{mvcc::RecordKeyVisitor, read_control::StorageReadControl};

use super::{
    codec, info, runs, CommitSequence, PhysicalResult, RecordMetadata, Snapshot, VersionError,
    VersionResult,
};
use crate::read_control::{copy_bytes, payload_length, prefix_upper_bound, reserve_bindings};

pub(super) fn visit(
    snapshot: &Snapshot,
    prefix: &[u8],
    before: Option<&[u8]>,
    control: &StorageReadControl,
    visit: &mut RecordKeyVisitor<'_>,
) -> VersionResult<()> {
    control.check()?;
    let upper =
        prefix_upper_bound(prefix, control).map_err(|error| VersionError::Storage(error.into()))?;
    let before = match (before, upper.as_deref()) {
        (Some(before), Some(upper)) => Some(before.min(upper)),
        (before, upper) => before.or(upper),
    };
    if before.is_some_and(|before| before <= prefix) {
        return Ok(());
    }
    snapshot.read(|connection| {
        let has_runs: bool = connection
            .prepare_cached("SELECT EXISTS(SELECT 1 FROM _uqa_mvcc_runs)")?
            .query_row([], |row| row.get(0))?;
        let mut cursor: Option<BudgetedVec<u8>> = None;
        loop {
            control.check().map_err(VersionError::from)?;
            let bound = cursor.as_deref().or(before);
            let point = previous_point(connection, prefix, bound, control)?;
            let run = if has_runs {
                runs::previous_key(connection, prefix, bound, control)?
            } else {
                None
            };
            let key = match (point, run) {
                (Some(point), Some(run)) if point[..] >= run[..] => Some(point),
                (_, Some(run)) => Some(run),
                (point, None) => point,
            };
            let Some(key) = key else { break };
            let _bindings = reserve_bindings(control, &[&key])?;
            if let Some(info) = info(connection, &key, snapshot.sequence)? {
                visit(
                    &key,
                    RecordMetadata {
                        revision: Some(CommitSequence::from_u64(info.revision)),
                        live: info.length.is_some(),
                    },
                )?;
                break;
            }
            // A key created after the pinned boundary is not visible. Seek below it without advancing the snapshot.
            cursor = Some(key);
        }
        control.check().map_err(VersionError::from)?;
        Ok(())
    })
}

fn previous_point(
    connection: &Connection,
    prefix: &[u8],
    before: Option<&[u8]>,
    control: &StorageReadControl,
) -> PhysicalResult<Option<BudgetedVec<u8>>> {
    let _bindings = reserve_bindings(control, &[prefix, before.unwrap_or_default()])?;
    let (sizes, data) = if before.is_some() {
        ("SELECT length(key) FROM _uqa_mvcc_heads WHERE key >= ?1 AND key < ?2 ORDER BY key DESC LIMIT 1", "SELECT key FROM _uqa_mvcc_heads WHERE key >= ?1 AND key < ?2 ORDER BY key DESC LIMIT 1")
    } else {
        ("SELECT length(key) FROM _uqa_mvcc_heads WHERE key >= ?1 AND (?2 IS NULL) ORDER BY key DESC LIMIT 1", "SELECT key FROM _uqa_mvcc_heads WHERE key >= ?1 AND (?2 IS NULL) ORDER BY key DESC LIMIT 1")
    };
    let length: Option<i64> = connection
        .prepare_cached(sizes)?
        .query_row(params![prefix, before], |row| row.get(0))
        .optional()?;
    let Some(length) = length else {
        return Ok(None);
    };
    let length = payload_length(length)?;
    let _payload = control
        .memory()
        .reserve(length)
        .map_err(VersionError::from)?;
    control.check().map_err(VersionError::from)?;
    let mut statement = connection.prepare_cached(data)?;
    let mut rows = statement.query(params![prefix, before])?;
    let row = rows.next()?.ok_or(VersionError::InvalidEncoding(
        "head disappeared within a read",
    ))?;
    let key = codec::bytes(row, 0)?;
    if key.len() != length || !key.starts_with(prefix) {
        return Err(VersionError::InvalidEncoding("head changed within a read").into());
    }
    Ok(Some(copy_bytes(key, 0, control)?))
}
