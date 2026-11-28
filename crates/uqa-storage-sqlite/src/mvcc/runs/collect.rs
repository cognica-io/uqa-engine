//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coalesce only current revisions at or before the oldest retained snapshot.

use rusqlite::{params, types::ValueRef, Connection, Row};
use uqa_storage::{
    mvcc::{CommitSequence, VersionError},
    read_control::StorageReadControl,
};

use super::{suffix, Bounds, PhysicalResult, Run, MAX_COUNT, MAX_KEY, MAX_VALUE};
use crate::mvcc::{codec, read};

pub(in crate::mvcc) fn compact(
    connection: &Connection,
    horizon: CommitSequence,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    let mut pending: Option<Run> = None;
    // Include an empty position for an ineligible key so it still ends a pending run. Only completed ranges behind the current key are removed.
    let mut scan = connection.prepare_cached(
        "SELECT h.sequence, h.compacted, m.sequence, m.payload_length,
             CASE WHEN h.compacted = 0 AND h.sequence <= ?1 AND m.payload_length BETWEEN 0 AND ?3 THEN
                 (SELECT v.value FROM _uqa_mvcc_versions v WHERE v.version_id = m.version_id AND v.key = h.key AND v.sequence = m.sequence AND length(v.value) <= ?3) END,
             CASE WHEN length(h.key) BETWEEN 8 AND ?2 THEN h.key END
         FROM _uqa_mvcc_heads h LEFT JOIN _uqa_mvcc_version_metadata m
           ON length(h.key) BETWEEN 8 AND ?2 AND m.key = h.key AND m.sequence =
                (SELECT max(p.sequence) FROM _uqa_mvcc_version_metadata p WHERE p.key = h.key)
         ORDER BY h.key",
    )?;
    let mut rows = scan.query(params![
        horizon.as_u64().to_be_bytes().as_slice(),
        i64::try_from(MAX_KEY).expect("bounded key length"),
        i64::try_from(MAX_VALUE).expect("bounded template")
    ])?;
    while let Some(row) = rows.next()? {
        control.check().map_err(VersionError::from)?;
        let next = candidate(row, horizon, control)?;
        if let (Some(run), Some(next)) = (pending.as_mut(), next.as_ref()) {
            if extend(run, next) {
                continue;
            }
        }
        if let Some(run) = pending.take() {
            persist(connection, &run, control)?;
        }
        pending = next;
    }
    if let Some(run) = pending {
        persist(connection, &run, control)?;
    }
    Ok(())
}

fn candidate(
    row: &Row<'_>,
    horizon: CommitSequence,
    control: &StorageReadControl,
) -> PhysicalResult<Option<Run>> {
    if matches!(row.get_ref(5)?, ValueRef::Null) {
        return Ok(None);
    }
    let key = codec::bytes(row, 5)?;
    let Some(info) = read::point_info(row, CommitSequence::from_u64(u64::MAX))? else {
        return Err(VersionError::InvalidEncoding("compaction head disappeared").into());
    };
    if info.revision > horizon.as_u64() || info.length.is_some_and(|length| length > MAX_VALUE) {
        return Ok(None);
    }
    let mut run = Run {
        bounds: Bounds {
            first: [0; MAX_KEY],
            last: [0; MAX_KEY],
            key_length: key.len(),
            sequence: CommitSequence::from_u64(info.revision),
            kind: i64::from(info.length.is_some()),
            value_length: info.length,
        },
        template: [0; MAX_VALUE],
    };
    run.bounds.first[..key.len()].copy_from_slice(key);
    run.bounds.last[..key.len()].copy_from_slice(key);
    if info.length.is_some() {
        read::inline_value(row, info, MAX_VALUE, control, &mut |record| {
            let value =
                record
                    .and_then(|record| record.value)
                    .ok_or(VersionError::InvalidEncoding(
                        "compaction record disappeared",
                    ))?;
            run.template[..value.len()].copy_from_slice(value);
            Ok(())
        })?;
    }
    Ok(Some(run))
}

fn extend(run: &mut Run, next: &Run) -> bool {
    let bounds = &run.bounds;
    let candidate = &next.bounds;
    let count = suffix(bounds.last()) - suffix(bounds.first()) + 1;
    if bounds.key_length != candidate.key_length
        || bounds.sequence != candidate.sequence
        || bounds.value_length != candidate.value_length
        || bounds.first()[..bounds.key_length - 8] != candidate.first()[..candidate.key_length - 8]
        || suffix(bounds.last()).checked_add(1) != Some(suffix(candidate.first()))
        || count == MAX_COUNT
    {
        return false;
    }
    let length = bounds.value_length.unwrap_or(0);
    let mut output = [0; MAX_VALUE];
    if run.value(candidate.first(), &mut output)
        != next
            .bounds
            .value_length
            .map(|length| &next.template[..length])
    {
        if count != 1 || length < 8 || run.template[..length - 8] != next.template[..length - 8] {
            return false;
        }
        let mask = suffix(bounds.first()) ^ suffix(&run.template[..length]);
        if mask != suffix(candidate.first()) ^ suffix(&next.template[..length]) {
            return false;
        }
        run.template[length - 8..length].copy_from_slice(&mask.to_be_bytes());
        run.bounds.kind = 2;
    }
    run.bounds.last = candidate.last;
    true
}

fn persist(connection: &Connection, run: &Run, control: &StorageReadControl) -> PhysicalResult<()> {
    if run.bounds.first() == run.bounds.last() {
        return Ok(());
    }
    let _bindings = crate::read_control::reserve_bindings(
        control,
        &[
            run.bounds.first(),
            run.bounds.last(),
            &run.template[..run.bounds.value_length.unwrap_or(0)],
        ],
    )?;
    control.cancellation().check().map_err(VersionError::from)?;
    run.insert(connection, run.bounds.first(), run.bounds.last())?;
    for sql in [
        "DELETE FROM _uqa_mvcc_versions WHERE version_id IN (SELECT version_id FROM _uqa_mvcc_version_metadata WHERE key >= ?1 AND key <= ?2 AND length(key) = ?3)",
        "DELETE FROM _uqa_mvcc_heads WHERE key >= ?1 AND key <= ?2 AND length(key) = ?3",
    ] {
        connection.prepare_cached(sql)?.execute(
            params![
                run.bounds.first(),
                run.bounds.last(),
                i64::try_from(run.bounds.key_length).expect("bounded key length")
            ],
        )?;
    }
    Ok(())
}
