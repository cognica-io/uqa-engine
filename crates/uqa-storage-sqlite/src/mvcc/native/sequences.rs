//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A sequence incarnation has one live definition generation, including across independent committers.

use rusqlite::{params, Connection};
use uqa_storage::{mvcc::PreparedRecordCommit, read_control::StorageReadControl};

use super::{invalid, NativeRecordFamily as Family, NativeRecordIdentity, NativeRecordOwner};
use crate::{
    mvcc::PhysicalResult,
    read_control::{prefix_upper_bound, reserve_bindings},
};

pub(super) fn validate_source(connection: &Connection) -> PhysicalResult<()> {
    if connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM _sequences GROUP BY object_id HAVING count(*) > 1)",
        [],
        |row| row.get::<_, bool>(0),
    )? {
        return Err(invalid("native sequences share an object identity").into());
    }
    Ok(())
}

pub(super) fn validate_prepared(
    connection: &Connection,
    prepared: &PreparedRecordCommit,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    for record in prepared.records() {
        let identity = NativeRecordIdentity::decode(record.key())?;
        if identity.family() != Family::Sequences || record.value().is_none() {
            continue;
        }
        let NativeRecordOwner::Object { identity, .. } = identity.owner() else {
            unreachable!("sequence definitions are object-owned")
        };
        let prefix = NativeRecordIdentity::object_prefix(Family::Sequences, identity, control)?;
        let upper = prefix_upper_bound(&prefix, control)?.expect("native prefix has a successor");
        let _bindings = reserve_bindings(control, &[&prefix, &upper, record.key()])?;
        // The final prepared definitions must be unique, and every other currently live generation must be retired by this exact batch. Probe ordered record keys without loading sequence ACLs or other payloads.
        let collision: bool = connection.query_row(
            "SELECT
                (SELECT count(*) > 1 FROM _uqa_mvcc_native_expected
                 WHERE new_key >= ?1 AND new_key < ?2)
                OR EXISTS (
                    SELECT 1 FROM _uqa_mvcc_heads AS head
                    JOIN _uqa_mvcc_versions AS version
                      ON version.key = head.key AND version.sequence = head.sequence
                    WHERE head.key >= ?1 AND head.key < ?2 AND head.key != ?3
                      AND version.value IS NOT NULL
                      AND NOT EXISTS (
                          SELECT 1 FROM _uqa_mvcc_native_expected AS retired
                          WHERE retired.old_key = head.key AND retired.new_key IS NOT head.key
                      )
                )",
            params![&prefix[..], &upper[..], record.key()],
            |row| row.get(0),
        )?;
        if collision {
            return Err(invalid("native sequence incarnation has competing definitions").into());
        }
    }
    Ok(())
}
