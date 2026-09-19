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
    family: Family,
    row: &[ValueRef<'_>],
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    if !source_projects(connection, family, row, control)? {
        return Err(invalid("native graph lookup has no matching source entity").into());
    }
    Ok(())
}

fn source_projects(
    connection: &Connection,
    family: Family,
    lookup: &[ValueRef<'_>],
    control: &StorageReadControl,
) -> PhysicalResult<bool> {
    let scoped = family == Family::StandaloneGraphLookups;
    let prefix = if scoped { &lookup[..1] } else { &[] };
    let entire_lookup = lookup;
    let lookup = &lookup[usize::from(scoped)..];
    let kind = lookup[0]
        .as_str()
        .map_err(|_| invalid("graph selector is not text"))?;
    let entity = lookup[3]
        .as_str()
        .map_err(|_| invalid("graph selector is not text"))?;
    let source = match (kind, entity) {
        ("label", "vertex") if scoped => Family::StandaloneGraphVertices,
        ("label" | "source" | "target", "edge") if scoped => Family::StandaloneGraphEdges,
        ("member", "vertex" | "edge") if scoped => Family::StandaloneGraphMembership,
        ("label", "vertex") => Family::GraphVertices,
        ("label" | "source" | "target", "edge") => Family::GraphEdges,
        ("member", _) if !scoped => Family::GraphMembership,
        ("path", _) if !scoped => Family::GraphPathIndexState,
        _ => return Err(invalid("unknown native graph lookup kind").into()),
    };
    let mut parts = [ValueRef::Null; 4];
    parts[..prefix.len()].copy_from_slice(prefix);
    let key_parts: &[ValueRef<'_>] = if source == Family::GraphPathIndexState {
        &[lookup[3]]
    } else if matches!(
        source,
        Family::GraphMembership | Family::StandaloneGraphMembership
    ) {
        &[lookup[3], lookup[4], lookup[1]]
    } else {
        &[lookup[4]]
    };
    parts[prefix.len()..prefix.len() + key_parts.len()].copy_from_slice(key_parts);
    let key = encode_row(&parts[..prefix.len() + key_parts.len()], control)?;
    let Some(row) = physical::get(connection, source.layout(), &key, control)? else {
        return Ok(false);
    };
    let values = decode_row(&row, source.layout().columns.len(), control)?;
    let mut matches = false;
    rows(source, &values, |projected| {
        matches |= projected == entire_lookup;
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
        let family = NativeRecordIdentity::decode(record.key())?.family();
        if record.value().is_some()
            || !matches!(
                family,
                Family::GraphLookups | Family::StandaloneGraphLookups
            )
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
                if source_projects(connection, family, &values, control)
                    .map_err(Error::into_version)?
                {
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
