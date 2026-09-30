//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Paired ordered cursors admit each encoded key before fetching it, then merge compacted runs.

use rusqlite::{params, Connection};
use uqa_core::memory::{BudgetedVec, MemoryReservation};
use uqa_storage::read_control::StorageReadControl;

use super::{codec, info, point_info, runs, CommitSequence, Info, PhysicalResult, VersionError};
use crate::read_control::{payload_length, prefix_upper_bound, reserve_bindings};

pub(super) fn visit(
    connection: &Connection,
    prefix: &[u8],
    after: Option<&[u8]>,
    limit: usize,
    boundary: CommitSequence,
    control: &StorageReadControl,
    visit: &mut impl FnMut(&[u8], Info) -> PhysicalResult<bool>,
) -> PhysicalResult<()> {
    control.check().map_err(VersionError::from)?;
    if limit == 0 {
        return Ok(());
    }
    let upper = prefix_upper_bound(prefix, control)?;
    let has_runs: bool = connection
        .prepare_cached("SELECT EXISTS(SELECT 1 FROM _uqa_mvcc_runs)")?
        .query_row([], |row| row.get(0))?;
    let mut pending = if has_runs {
        next_run(connection, prefix, after, upper.as_deref(), control)?
    } else {
        None
    };
    let mut count = 0;
    let mut running = true;
    let mut emit = |key: &[u8], record: Info| -> PhysicalResult<bool> {
        control.check().map_err(VersionError::from)?;
        count += 1;
        let more = visit(key, record)?;
        control.check().map_err(VersionError::from)?;
        Ok(more && count < limit)
    };
    points(
        connection,
        prefix,
        after,
        upper.as_deref(),
        boundary,
        control,
        &mut |key, record| {
            while let Some(run) = pending.as_ref().filter(|run| &***run < key) {
                if let Some(record) = run_info(connection, run, boundary, control)? {
                    running = emit(run, record)?;
                    if !running {
                        return Ok(false);
                    }
                }
                pending = next_run(connection, prefix, Some(run), upper.as_deref(), control)?;
            }
            if pending.as_deref() == Some(key) {
                // Point heads keep the same precedence as individual record reads.
                pending = next_run(connection, prefix, Some(key), upper.as_deref(), control)?;
            }
            if let Some(record) = record {
                running = emit(key, record)?;
            }
            Ok(running)
        },
    )?;
    while running {
        let Some(run) = pending else { break };
        if let Some(record) = run_info(connection, &run, boundary, control)? {
            running = emit(&run, record)?;
        }
        if running {
            pending = next_run(connection, prefix, Some(&run), upper.as_deref(), control)?;
        } else {
            pending = None;
        }
    }
    control.check().map_err(VersionError::from)?;
    Ok(())
}

fn run_info(
    connection: &Connection,
    key: &[u8],
    boundary: CommitSequence,
    control: &StorageReadControl,
) -> PhysicalResult<Option<Info>> {
    let _bindings = reserve_bindings(control, &[key])?;
    info(connection, key, boundary)
}

fn next_run(
    connection: &Connection,
    prefix: &[u8],
    after: Option<&[u8]>,
    upper: Option<&[u8]>,
    control: &StorageReadControl,
) -> PhysicalResult<Option<BudgetedVec<u8>>> {
    let after = after.filter(|after| *after >= prefix);
    runs::next_key(
        connection,
        after.unwrap_or(prefix),
        after.is_some(),
        upper,
        control,
    )
}

fn points(
    connection: &Connection,
    prefix: &[u8],
    after: Option<&[u8]>,
    upper: Option<&[u8]>,
    boundary: CommitSequence,
    control: &StorageReadControl,
    visit: &mut impl FnMut(&[u8], Option<Info>) -> PhysicalResult<bool>,
) -> PhysicalResult<()> {
    let after = after.filter(|after| *after >= prefix);
    let lower = after.unwrap_or(prefix);
    macro_rules! metadata {
        ($predicate:literal) => { concat!("SELECT h.sequence, h.compacted, v.sequence, CASE WHEN v.value IS NULL THEN NULL WHEN typeof(v.value) = 'blob' THEN length(v.value) ELSE -1 END, h.key FROM _uqa_mvcc_heads h LEFT JOIN _uqa_mvcc_versions v ON v.key = h.key AND v.sequence = (SELECT sequence FROM _uqa_mvcc_versions WHERE key = h.key AND sequence <= ?3 ORDER BY sequence DESC LIMIT 1) WHERE ", $predicate, " ORDER BY h.key") };
    }
    let (sizes, data) = match (after.is_some(), upper.is_some()) {
        (true, true) => (
            "SELECT length(key) FROM _uqa_mvcc_heads WHERE key > ?1 AND key < ?2 ORDER BY key",
            metadata!("h.key > ?1 AND h.key < ?2"),
        ),
        (true, false) => (
            "SELECT length(key) FROM _uqa_mvcc_heads WHERE key > ?1 AND (?2 IS NULL) ORDER BY key",
            metadata!("h.key > ?1 AND (?2 IS NULL)"),
        ),
        (false, true) => (
            "SELECT length(key) FROM _uqa_mvcc_heads WHERE key >= ?1 AND key < ?2 ORDER BY key",
            metadata!("h.key >= ?1 AND h.key < ?2"),
        ),
        (false, false) => (
            "SELECT length(key) FROM _uqa_mvcc_heads WHERE key >= ?1 AND (?2 IS NULL) ORDER BY key",
            metadata!("h.key >= ?1 AND (?2 IS NULL)"),
        ),
    };
    let sequence = boundary.as_u64().to_be_bytes();
    let _size_bindings = reserve_bindings(control, &[lower, upper.unwrap_or_default()])?;
    let _data_bindings = reserve_bindings(control, &[lower, upper.unwrap_or_default()])?;
    // Keep the previous row charged until stepping releases its SQLite-owned key buffer, including error cleanup.
    let mut previous_payload: Option<MemoryReservation> = None;
    let mut size_statement = connection.prepare_cached(sizes)?;
    let mut data_statement = connection.prepare_cached(data)?;
    let mut size_rows = size_statement.query(params![lower, upper])?;
    let mut data_rows = data_statement.query(params![lower, upper, sequence.as_slice()])?;
    while let Some(size) = size_rows.next()? {
        control.check().map_err(VersionError::from)?;
        let length = payload_length(size.get(0)?)?;
        let payload = control
            .memory()
            .reserve(length)
            .map_err(VersionError::from)?;
        let row = data_rows.next()?.ok_or(VersionError::InvalidEncoding(
            "head disappeared within a read",
        ))?;
        previous_payload = Some(payload);
        let key = codec::bytes(row, 4)?;
        if key.len() != length || !key.starts_with(prefix) {
            return Err(VersionError::InvalidEncoding("head changed within a read").into());
        }
        if !visit(key, point_info(row, boundary)?)? {
            break;
        }
    }
    // Both statements are finalized before their last borrowed key is released from the allowance.
    drop(data_rows);
    drop(data_statement);
    drop(size_rows);
    drop(size_statement);
    drop(previous_payload);
    control.check().map_err(VersionError::from)?;
    Ok(())
}
