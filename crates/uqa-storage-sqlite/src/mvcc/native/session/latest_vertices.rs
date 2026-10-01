//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Committed catalog graph vertices read from `_graph_vertices`, the projection of their records, in one B-tree pass for a batch of identities.

use rusqlite::{params, types::ValueRef, Connection, Row};
use uqa_core::memory::{MemoryError, MemoryReservation};
use uqa_storage::mvcc::VersionError;
use uqa_storage::read_control::StorageReadControl;

use super::NativeSnapshot;
use crate::connection::Result;
use crate::read_control::payload_length;

/// Properties of at most this many bytes are read with their row. The selected byte length bounds `SQLite`'s copy before the properties are evaluated; larger properties are admitted and then read by themselves.
const INLINE_PROPERTIES_BYTES: u16 = 16 * 1024;

/// Receives each requested identity with its row, or `None` when the vertex is missing, and returns whether to continue.
type VertexVisitor<'a> = dyn FnMut(i64, Option<&[ValueRef<'_>]>) -> Result<bool> + 'a;

const RANGE: &str = "SELECT vertex_id, label, octet_length(label) + octet_length(properties_json), CASE WHEN octet_length(properties_json) <= ?3 THEN properties_json END FROM _graph_vertices WHERE vertex_id BETWEEN ?1 AND ?2 ORDER BY vertex_id";
const POINT: &str = "SELECT vertex_id, label, octet_length(label) + octet_length(properties_json), CASE WHEN octet_length(properties_json) <= ?2 THEN properties_json END FROM _graph_vertices WHERE vertex_id = ?1";
const PROPERTIES: &str = "SELECT properties_json FROM _graph_vertices WHERE vertex_id = ?1";

impl NativeSnapshot {
    /// Visit the latest committed catalog graph vertices `ids`, which must ascend without duplicates, in `ids` order as `[vertex_id, label, properties_json]` rows, or `None` for a missing vertex, while `visit` returns true. Returns how many identities were visited, or `None` without visiting when the physical rows cannot stand in for this snapshot's records.
    pub(crate) fn visit_latest_vertices(
        &self,
        ids: &[i64],
        control: &StorageReadControl,
        visit: &mut VertexVisitor<'_>,
    ) -> Result<Option<usize>> {
        let (Some(first), Some(last)) = (ids.first(), ids.last()) else {
            return Ok(Some(0));
        };
        if ids.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Ok(None);
        }
        // A batch covering at least half of its identity span reads the span once; a sparser one seeks each identity.
        let span = last.abs_diff(*first).saturating_add(1);
        let dense = u64::try_from(ids.len())
            .unwrap_or(u64::MAX)
            .saturating_mul(2)
            >= span;
        self.read_latest_projection(control, &mut |connection| {
            let visited = if dense {
                visit_range(connection, ids, control, visit)
            } else {
                visit_points(connection, ids, control, visit)
            };
            visited
                .map(Some)
                .map_err(|error| error.into_version().into())
        })
    }
}

fn visit_range(
    connection: &Connection,
    ids: &[i64],
    control: &StorageReadControl,
    visit: &mut VertexVisitor<'_>,
) -> crate::mvcc::PhysicalResult<usize> {
    // Declared before the statements so that every exit finalizes them first: a row's allowance is released only after stepping or finalization releases its SQLite-owned buffers.
    let mut admitted: Option<MemoryReservation> = None;
    let mut properties = connection.prepare_cached(PROPERTIES)?;
    let mut statement = connection.prepare_cached(RANGE)?;
    let mut rows = statement.query(params![
        ids[0],
        ids[ids.len() - 1],
        i64::from(INLINE_PROPERTIES_BYTES)
    ])?;
    let mut next = 0;
    let mut visited = 0;
    while let Some(row) = rows.next()? {
        control.check().map_err(VersionError::from)?;
        let vertex_id: i64 = row.get(0)?;
        while next < ids.len() && ids[next] < vertex_id {
            visited += 1;
            if !visit(ids[next], None)? {
                return Ok(visited);
            }
            next += 1;
        }
        if next == ids.len() {
            break;
        }
        if ids[next] != vertex_id {
            continue;
        }
        admitted = Some(admit(row, control)?);
        visited += 1;
        next += 1;
        if !visit_row(row, &mut properties, visit)? {
            return Ok(visited);
        }
    }
    drop(rows);
    while next < ids.len() {
        visited += 1;
        if !visit(ids[next], None)? {
            break;
        }
        next += 1;
    }
    drop(statement);
    drop(properties);
    drop(admitted);
    Ok(visited)
}

fn visit_points(
    connection: &Connection,
    ids: &[i64],
    control: &StorageReadControl,
    visit: &mut VertexVisitor<'_>,
) -> crate::mvcc::PhysicalResult<usize> {
    let mut admitted: Option<MemoryReservation> = None;
    let mut properties = connection.prepare_cached(PROPERTIES)?;
    let mut statement = connection.prepare_cached(POINT)?;
    let mut visited = 0;
    for id in ids {
        control.check().map_err(VersionError::from)?;
        let mut rows = statement.query(params![id, i64::from(INLINE_PROPERTIES_BYTES)])?;
        visited += 1;
        let more = match rows.next()? {
            Some(row) => {
                admitted = Some(admit(row, control)?);
                visit_row(row, &mut properties, visit)?
            }
            None => visit(*id, None)?,
        };
        if !more {
            break;
        }
    }
    drop(statement);
    drop(properties);
    drop(admitted);
    Ok(visited)
}

/// Charge a row's label and properties before borrowing them.
fn admit(
    row: &Row<'_>,
    control: &StorageReadControl,
) -> crate::mvcc::PhysicalResult<MemoryReservation> {
    let bytes = payload_length(row.get(2)?)?;
    Ok(control
        .memory()
        .reserve(bytes)
        .map_err(|error: MemoryError| VersionError::from(error))?)
}

fn visit_row(
    row: &Row<'_>,
    properties: &mut rusqlite::CachedStatement<'_>,
    visit: &mut VertexVisitor<'_>,
) -> crate::mvcc::PhysicalResult<bool> {
    let vertex_id: i64 = row.get(0)?;
    let label = row.get_ref(1)?;
    if let ValueRef::Text(_) = row.get_ref(3)? {
        return Ok(visit(
            vertex_id,
            Some(&[ValueRef::Integer(vertex_id), label, row.get_ref(3)?]),
        )?);
    }
    let mut rows = properties.query(params![vertex_id])?;
    let stored = rows.next()?.ok_or(VersionError::InvalidEncoding(
        "graph vertex disappeared within a read",
    ))?;
    Ok(visit(
        vertex_id,
        Some(&[ValueRef::Integer(vertex_id), label, stored.get_ref(0)?]),
    )?)
}
