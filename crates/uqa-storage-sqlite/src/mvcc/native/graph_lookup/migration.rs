//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Backfill selector revisions from complete source histories without changing their commit boundaries.

use rusqlite::{params, Connection};
use uqa_storage::{
    mvcc::{CommitSequence, DatabaseId, VersionError},
    read_control::StorageReadControl,
};

use super::{records, Family, NativeRecord, SOURCES};
use crate::mvcc::{
    codec,
    native::{decode_record, invalid, physical, NativeRecordIdentity, NativeRecordOwner},
    read, write, Error, PhysicalResult,
};

pub(in crate::mvcc::native) fn backfill(
    connection: &Connection,
    database: DatabaseId,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    let lookup = NativeRecordIdentity::family_prefix(Family::GraphLookups, control)?;
    require_history_heads(connection, &lookup, control)?;
    let mut present = false;
    read::keys(connection, &lookup, None, 1, control, &mut |_| {
        present = true;
        Ok(Some(false))
    })?;
    if present {
        return Err(invalid("native graph lookup history already exists before migration").into());
    }
    let boundary = codec::header(connection, database)?.sequence;
    for family in SOURCES {
        let prefix = NativeRecordIdentity::family_prefix(family, control)?;
        require_history_heads(connection, &prefix, control)?;
        read::keys(connection, &prefix, None, usize::MAX, control, &mut |key| {
            backfill_key(connection, database, family, key, boundary, control)?;
            Ok(Some(true))
        })?;
    }
    Ok(())
}

/// A source may now be deleted, but every retained revision must still be reachable through its head for historical readers and conversion.
fn require_history_heads(
    connection: &Connection,
    prefix: &[u8],
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    let upper = crate::read_control::prefix_upper_bound(prefix, control)?
        .ok_or_else(|| invalid("native family prefix has no upper bound"))?;
    let _bindings = crate::read_control::reserve_bindings(control, &[prefix, &upper])?;
    control.cancellation().check().map_err(VersionError::from)?;
    let orphaned: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM _uqa_mvcc_versions AS v WHERE v.key >= ?1 AND v.key < ?2 AND NOT EXISTS(SELECT 1 FROM _uqa_mvcc_heads AS h WHERE h.key = v.key))",
        params![prefix, &upper[..]],
        |row| row.get(0),
    )?;
    control.cancellation().check().map_err(VersionError::from)?;
    if orphaned {
        return Err(invalid("native graph history has a revision without its head").into());
    }
    Ok(())
}

fn backfill_key(
    connection: &Connection,
    database: DatabaseId,
    family: Family,
    key: &[u8],
    boundary: CommitSequence,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    let identity = NativeRecordIdentity::decode_full(key, control)?;
    if identity.owner() != NativeRecordOwner::Database(database) {
        return Err(VersionError::WrongDatabase.into());
    }
    let _bindings = crate::read_control::reserve_bindings(control, &[key])?;
    let head = codec::head(connection, key)?;
    let mut statement = connection
        .prepare("SELECT sequence FROM _uqa_mvcc_versions WHERE key = ?1 ORDER BY sequence")?;
    let mut versions = statement.query(params![key])?;
    let mut previous: [Option<NativeRecord>; 3] = [None, None, None];
    let mut last = None;
    while let Some(row) = versions.next()? {
        let sequence = CommitSequence::from_u64(codec::integer(codec::bytes(row, 0)?)?);
        if sequence.as_u64() == 0 || sequence > boundary {
            return Err(invalid("graph source history has an invalid commit boundary").into());
        }
        let mut current = [None, None, None];
        read::value(connection, key, sequence, control, &mut |record| {
            if let Some(bytes) = record.and_then(|record| record.value) {
                let (_, values) = decode_record(key, bytes, control)?;
                if Some(sequence) == head {
                    let physical_key = physical::physical_key(family.layout(), &values, control)
                        .map_err(Error::into_version)?;
                    let materialized =
                        physical::get(connection, family.layout(), &physical_key, control)
                            .map_err(Error::into_version)?;
                    if materialized.as_deref() != Some(bytes) {
                        return Err(invalid(
                            "native graph history has no matching current source entity",
                        ));
                    }
                }
                current =
                    records(database, family, &values, control).map_err(Error::into_version)?;
            }
            Ok(())
        })?;
        for old in previous.iter().flatten() {
            if !current.iter().flatten().any(|new| new.key() == old.key()) {
                write::stage_record(connection, old.key(), None, sequence, control)?;
            }
        }
        for new in current.iter().flatten() {
            if !previous.iter().flatten().any(|old| old.key() == new.key()) {
                write::stage_record(connection, new.key(), Some(new.row()), sequence, control)?;
            }
        }
        previous = current;
        last = Some(sequence);
    }
    if last.is_none() || last != head {
        return Err(invalid("graph source history disagrees with its current head").into());
    }
    Ok(())
}
