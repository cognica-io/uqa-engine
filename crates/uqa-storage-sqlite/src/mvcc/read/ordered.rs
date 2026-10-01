//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Paired ordered cursors admit each encoded key before fetching it, then merge compacted runs.

use rusqlite::{params, Connection};
use uqa_core::memory::MemoryReservation;
use uqa_storage::read_control::StorageReadControl;

use super::{codec, info, runs, CommitSequence, Info, PhysicalResult, VersionError};
use crate::read_control::{payload_length, prefix_upper_bound, reserve_bindings};

mod metadata;

#[cfg(test)]
mod tests;

#[derive(Clone, Copy)]
pub(super) enum RecordSource {
    Point(Option<Info>),
    Run(Info),
}

impl RecordSource {
    fn info(self) -> Option<Info> {
        match self {
            Self::Point(info) => info,
            Self::Run(info) => Some(info),
        }
    }
}

pub(super) fn visit(
    connection: &Connection,
    prefix: &[u8],
    after: Option<&[u8]>,
    limit: usize,
    boundary: CommitSequence,
    control: &StorageReadControl,
    visit: &mut impl FnMut(&[u8], Info) -> PhysicalResult<bool>,
) -> PhysicalResult<()> {
    visit_sources(
        connection,
        prefix,
        after,
        limit,
        boundary,
        control,
        &mut |key, source| source.info().map_or(Ok(true), |info| visit(key, info)),
    )
}

/// Include invisible point heads so a paired payload cursor advances only after metadata admission.
pub(super) fn visit_sources(
    connection: &Connection,
    prefix: &[u8],
    after: Option<&[u8]>,
    limit: usize,
    boundary: CommitSequence,
    control: &StorageReadControl,
    visit: &mut impl FnMut(&[u8], RecordSource) -> PhysicalResult<bool>,
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
    let mut emit = |key: &[u8], source: RecordSource| -> PhysicalResult<bool> {
        control.check().map_err(VersionError::from)?;
        count += usize::from(source.info().is_some());
        let more = visit(key, source)?;
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
            while let Some(run) = pending.as_ref().filter(|run| run.bytes() < key) {
                let _payload = control
                    .memory()
                    .reserve(run.bytes().len())
                    .map_err(VersionError::from)?;
                if let Some(record) = run_info(connection, run.bytes(), boundary, control)? {
                    running = emit(run.bytes(), RecordSource::Run(record))?;
                    if !running {
                        return Ok(false);
                    }
                }
                pending = next_run(
                    connection,
                    prefix,
                    Some(run.bytes()),
                    upper.as_deref(),
                    control,
                )?;
            }
            let same_key = pending.as_ref().is_some_and(|run| run.bytes() == key);
            running = emit(key, RecordSource::Point(record))?;
            if running && same_key {
                // Point heads keep precedence; advance a tied run only if the consumer continues.
                pending = next_run(connection, prefix, Some(key), upper.as_deref(), control)?;
            }
            Ok(running)
        },
    )?;
    while running {
        let Some(run) = pending else { break };
        let _payload = control
            .memory()
            .reserve(run.bytes().len())
            .map_err(VersionError::from)?;
        if let Some(record) = run_info(connection, run.bytes(), boundary, control)? {
            running = emit(run.bytes(), RecordSource::Run(record))?;
        }
        if running {
            pending = next_run(
                connection,
                prefix,
                Some(run.bytes()),
                upper.as_deref(),
                control,
            )?;
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
) -> PhysicalResult<Option<runs::KeyCandidate>> {
    let after = after.filter(|after| *after >= prefix);
    runs::next_candidate(
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
    // Always seek the predecessor, including for a visible head. Only after
    // equality is checked may its revision be represented by the head itself.
    let (sizes, data) = match (after.is_some(), upper.is_some()) {
        (true, true) => (
            "SELECT length(key), CASE WHEN sequence > ?3 THEN 1 ELSE 0 END FROM _uqa_mvcc_heads WHERE key > ?1 AND key < ?2 ORDER BY key",
            metadata::statement!("h.key", "h.key > ?1 AND h.key < ?2"),
        ),
        (true, false) => (
            "SELECT length(key), CASE WHEN sequence > ?3 THEN 1 ELSE 0 END FROM _uqa_mvcc_heads WHERE key > ?1 AND (?2 IS NULL) ORDER BY key",
            metadata::statement!("h.key", "h.key > ?1 AND (?2 IS NULL)"),
        ),
        (false, true) => (
            "SELECT length(key), CASE WHEN sequence > ?3 THEN 1 ELSE 0 END FROM _uqa_mvcc_heads WHERE key >= ?1 AND key < ?2 ORDER BY key",
            metadata::statement!("h.key", "h.key >= ?1 AND h.key < ?2"),
        ),
        (false, false) => (
            "SELECT length(key), CASE WHEN sequence > ?3 THEN 1 ELSE 0 END FROM _uqa_mvcc_heads WHERE key >= ?1 AND (?2 IS NULL) ORDER BY key",
            metadata::statement!("h.key", "h.key >= ?1 AND (?2 IS NULL)"),
        ),
    };
    let sequence = boundary.as_u64().to_be_bytes();
    let _size_bindings = reserve_bindings(control, &[lower, upper.unwrap_or_default()])?;
    let _data_bindings = reserve_bindings(control, &[lower, upper.unwrap_or_default()])?;
    // Keep the previous row charged until stepping releases its SQLite-owned key buffer, including error cleanup.
    let mut previous_payload: Option<MemoryReservation> = None;
    let mut size_statement = connection.prepare_cached(sizes)?;
    let mut data_statement = connection.prepare_cached(data)?;
    let mut size_rows = size_statement.query(params![lower, upper, sequence.as_slice()])?;
    let mut data_rows = data_statement.query(params![lower, upper, sequence.as_slice()])?;
    while let Some(size) = size_rows.next()? {
        control.check().map_err(VersionError::from)?;
        let length = payload_length(size.get(0)?)?;
        let metadata_bytes = if size.get::<_, bool>(1)? {
            metadata::MAX_BYTES
        } else {
            0
        };
        let payload = control
            .memory()
            .reserve(
                length
                    .checked_add(metadata_bytes)
                    .ok_or(uqa_core::memory::MemoryError::SizeOverflow)
                    .map_err(VersionError::from)?,
            )
            .map_err(VersionError::from)?;
        let row = data_rows.next()?.ok_or(VersionError::InvalidEncoding(
            "head disappeared within a read",
        ))?;
        previous_payload = Some(payload);
        let key = codec::bytes(row, 3)?;
        if key.len() != length || !key.starts_with(prefix) {
            return Err(VersionError::InvalidEncoding("head changed within a read").into());
        }
        let record = metadata::info(row, boundary)?;
        if !visit(key, record)? {
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
