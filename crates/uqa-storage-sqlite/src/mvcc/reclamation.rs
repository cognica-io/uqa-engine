//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Delete only revisions older than each retained predecessor; current materialization, heads and receipts stay intact.

use super::{admission, codec, native, read, PhysicalResult};
use rusqlite::{params, Connection};
use uqa_storage::mvcc::{CommitSequence, DatabaseId, ReclamationHorizon, VersionError};
use uqa_storage::read_control::StorageReadControl;

pub(super) fn reclaim(
    connection: &Connection,
    identity: DatabaseId,
    mapped: Option<native::NativeRecordNamespace>,
    oldest: Option<CommitSequence>,
    control: &StorageReadControl,
) -> PhysicalResult<u64> {
    let _permit = admission::permit(connection, control)?;
    let transaction = admission::begin(connection, control)?;
    native::check_mapping(&transaction, mapped)?;
    let current = codec::header(&transaction, identity)?.sequence;
    let horizon = ReclamationHorizon::new(current, oldest)?;
    let mut removed = 0_u64;
    {
        let mut anchor = transaction.prepare(
            "SELECT max(sequence) FROM _uqa_mvcc_versions WHERE key = ?1 AND sequence <= ?2",
        )?;
        let mut delete = transaction.prepare("DELETE FROM _uqa_mvcc_versions WHERE key = ?1 AND sequence IN (SELECT sequence FROM _uqa_mvcc_versions WHERE key = ?1 AND sequence < ?2 ORDER BY sequence LIMIT 128)")?;
        read::point_keys(&transaction, b"", None, usize::MAX, control, &mut |key| {
            let (head, compacted) = codec::head_state(&transaction, key)?.ok_or(
                VersionError::InvalidEncoding("reclamation head disappeared"),
            )?;
            let _bindings = crate::read_control::reserve_bindings(control, &[key])?;
            let cutoff = if compacted && head <= horizon.sequence() {
                Some(head.as_u64())
            } else {
                let mut rows = anchor.query(params![
                    key,
                    horizon.sequence().as_u64().to_be_bytes().as_slice()
                ])?;
                let row = rows.next()?.ok_or(VersionError::InvalidEncoding(
                    "missing reclamation anchor result",
                ))?;
                match row.get_ref(0)? {
                    rusqlite::types::ValueRef::Null => None,
                    _ => Some(codec::integer(codec::bytes(row, 0)?)?),
                }
            };
            let Some(cutoff) = cutoff else {
                return Ok(Some(true));
            };
            let cutoff = horizon.anchor(CommitSequence::from_u64(cutoff))?;
            loop {
                control.cancellation().check().map_err(VersionError::from)?;
                let count =
                    delete.execute(params![key, cutoff.as_u64().to_be_bytes().as_slice()])?;
                removed =
                    removed
                        .checked_add(count as u64)
                        .ok_or(VersionError::InvalidEncoding(
                            "reclaimed version count overflow",
                        ))?;
                if count < 128 {
                    break;
                }
            }
            if !compacted && cutoff == head {
                compact_tombstone(&transaction, key, cutoff)?;
            }
            Ok(Some(true))
        })?;
    }
    super::runs::compact(&transaction, horizon.sequence(), control)?;
    admission::commit(transaction, control)?;
    Ok(removed)
}

fn compact_tombstone(
    connection: &Connection,
    key: &[u8],
    sequence: CommitSequence,
) -> PhysicalResult<()> {
    let sequence = sequence.as_u64().to_be_bytes();
    let changed = connection.execute("UPDATE _uqa_mvcc_heads SET compacted = 1 WHERE key = ?1 AND sequence = ?2 AND EXISTS(SELECT 1 FROM _uqa_mvcc_versions WHERE key = ?1 AND sequence = ?2 AND value IS NULL)", params![key, sequence.as_slice()])?;
    if changed != 0 {
        connection.execute(
            "DELETE FROM _uqa_mvcc_versions WHERE key = ?1 AND sequence = ?2 AND value IS NULL",
            params![key, sequence.as_slice()],
        )?;
    }
    Ok(())
}
