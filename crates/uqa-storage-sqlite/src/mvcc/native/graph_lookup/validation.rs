//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selector records cannot be inserted, changed or removed independently of their source entity.

use rusqlite::{types::ValueRef, Connection};
use uqa_storage::{
    mvcc::{CommitSequence, PreparedRecordCommit},
    read_control::StorageReadControl,
};

use super::{rows, Family};
use crate::mvcc::{
    native::{decode_record, decode_row, encode_row, invalid, physical, NativeRecordIdentity},
    read, Error, PhysicalResult,
};

pub(in crate::mvcc::native) fn validate_row(
    connection: &Connection,
    row: &[ValueRef<'_>],
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    if !source_projects(connection, row, control)? {
        return Err(invalid("native graph lookup has no matching source entity").into());
    }
    Ok(())
}

fn source_projects(
    connection: &Connection,
    lookup: &[ValueRef<'_>],
    control: &StorageReadControl,
) -> PhysicalResult<bool> {
    let kind = lookup[0]
        .as_str()
        .map_err(|_| invalid("graph selector is not text"))?;
    let entity = lookup[3]
        .as_str()
        .map_err(|_| invalid("graph selector is not text"))?;
    let source = match (kind, entity) {
        ("label", "vertex") => Family::GraphVertices,
        ("label" | "source" | "target", "edge") => Family::GraphEdges,
        ("member", _) => Family::GraphMembership,
        ("path", _) => Family::GraphPathIndexState,
        _ => return Err(invalid("unknown native graph lookup kind").into()),
    };
    let key = if source == Family::GraphPathIndexState {
        encode_row(&[lookup[3]], control)?
    } else if source == Family::GraphMembership {
        encode_row(&[lookup[3], lookup[4], lookup[1]], control)?
    } else {
        encode_row(&[lookup[4]], control)?
    };
    let Some(row) = physical::get(connection, source.layout(), &key, control)? else {
        return Ok(false);
    };
    let values = decode_row(&row, source.layout().columns.len(), control)?;
    let mut matches = false;
    rows(source, &values, |projected| {
        matches |= projected == lookup;
        Ok(())
    })?;
    Ok(matches)
}

pub(in crate::mvcc::native) fn validate_deletions(
    connection: &Connection,
    prepared: &PreparedRecordCommit,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    for record in prepared.records() {
        if record.value().is_some()
            || NativeRecordIdentity::decode(record.key())?.family() != Family::GraphLookups
        {
            continue;
        }
        read::value(
            connection,
            record.key(),
            CommitSequence::from_u64(u64::MAX),
            control,
            &mut |old| {
                let Some(bytes) = old.and_then(|old| old.value) else {
                    return Ok(());
                };
                let (_, values) = decode_record(record.key(), bytes, control)?;
                if source_projects(connection, &values, control).map_err(Error::into_version)? {
                    return Err(invalid(
                        "native graph lookup removal leaves a matching source entity",
                    ));
                }
                Ok(())
            },
        )?;
    }
    Ok(())
}
