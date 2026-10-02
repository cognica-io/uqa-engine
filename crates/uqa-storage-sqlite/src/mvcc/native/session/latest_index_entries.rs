//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A B-tree index's entries read from their physical table at a snapshot that is the latest commit, with the session's private records of them merged in.
//!
//! The rows of `_btree_index_entries` under a table's name belong to the owner bound to that name, as its `_documents` rows do, so when that owner has no other bound name the stored entries of one field, in document order, stand in for a scan of their records. A private record before a stored entry inserts one, and a private record at a stored entry replaces or deletes it.

use rusqlite::{params, types::ValueRef, Connection};
use uqa_storage::mvcc::VersionError;
use uqa_storage::read_control::StorageReadControl;

use super::latest_documents::stores_alone;
use super::private_rows::PrivateRows;
use super::{Family, NativeRecordIdentity, NativeRecordOwner, NativeSnapshot};
use crate::connection::Result;
use crate::mvcc::PhysicalResult;
use crate::read_control::reserve_bindings;

const ENTRIES: &str = "SELECT doc_id, value_json FROM _btree_index_entries WHERE table_name = ?1 AND field = ?2 ORDER BY doc_id";

impl NativeSnapshot {
    /// Visit the entries of index `field` of `table`, whose owner is `owner`, as document identity and encoded value in document order while `visit` returns true, when the latest committed rows stand in for this snapshot's records. Returns false without visiting otherwise; the caller then reads the records. `visit` runs inside the physical read and must not read the snapshot.
    pub(crate) fn visit_latest_index_entries(
        &self,
        table: &str,
        owner: NativeRecordOwner,
        field: ValueRef<'_>,
        control: &StorageReadControl,
        visit: &mut dyn FnMut(i64, &str) -> Result<bool>,
    ) -> Result<bool> {
        self.control.check()?;
        control.check()?;
        let entries = NativeRecordIdentity::new(Family::BtreeIndexEntries, owner)?;
        // A session that holds no private record at all skips the seek for one under the field's prefix.
        let private = self.view.private_revision().is_some()
            && !self
                .view
                .private_keys(&entries.encode_prefix(&[field], control)?, None, 1, control)?
                .is_empty();
        let visited = self.read_latest_committed(control, &mut |connection| {
            if !stores_alone(connection, table, owner, control)? {
                return Ok(None);
            }
            let private = private
                .then(|| PrivateRows::after(&self.view, entries, &[field], None, control))
                .transpose()?;
            visit_entries(connection, table, field, private, control, visit)
                .map_err(crate::mvcc::Error::into_version)?;
            Ok(Some(()))
        })?;
        Ok(visited.is_some())
    }
}

fn visit_entries(
    connection: &Connection,
    table: &str,
    field: ValueRef<'_>,
    mut private: Option<PrivateRows<'_>>,
    control: &StorageReadControl,
    visit: &mut dyn FnMut(i64, &str) -> Result<bool>,
) -> PhysicalResult<()> {
    let (ValueRef::Text(field_bytes) | ValueRef::Blob(field_bytes)) = field else {
        return Err(
            VersionError::InvalidEncoding("native B-tree field must be text or blob").into(),
        );
    };
    let _bindings = reserve_bindings(control, &[table.as_bytes(), field_bytes])?;
    let mut statement = connection.prepare_cached(ENTRIES)?;
    let mut rows = statement.query(params![
        table,
        rusqlite::types::ToSqlOutput::Borrowed(field)
    ])?;
    let mut stopped = false;
    while let Some(row) = rows.next()? {
        control.check().map_err(VersionError::from)?;
        let id: i64 = row.get(0)?;
        let mut replaced = false;
        if let Some(private) = private.as_mut() {
            while let Some(next) = private.peek().filter(|next| *next <= id) {
                replaced |= next == id;
                if visit_private(private, visit)? == Some(false) {
                    stopped = true;
                    break;
                }
            }
        }
        if stopped {
            break;
        }
        if replaced {
            continue;
        }
        let value = row
            .get_ref(1)?
            .as_str()
            .map_err(|_| VersionError::InvalidEncoding("native B-tree value must be text"))?;
        if !visit(id, value)? {
            stopped = true;
            break;
        }
    }
    drop(rows);
    drop(statement);
    if let Some(private) = private.as_mut().filter(|_| !stopped) {
        while private.peek().is_some() {
            control.check().map_err(VersionError::from)?;
            if visit_private(private, visit)? == Some(false) {
                break;
            }
        }
    }
    control.check().map_err(VersionError::from)?;
    Ok(())
}

/// Visit the current private entry, a `_btree_index_entries` row in its column order, and advance past it. Returns `None` for a deletion, which has no entry, and otherwise what `visit` returned.
fn visit_private(
    private: &mut PrivateRows<'_>,
    visit: &mut dyn FnMut(i64, &str) -> Result<bool>,
) -> PhysicalResult<Option<bool>> {
    let more = private.visit(&mut |row| {
        let id = row[2].as_i64().map_err(|_| {
            crate::SQLiteError::StorageBackend("native B-tree document id must be integer".into())
        })?;
        let value = row[3].as_str().map_err(|_| {
            crate::SQLiteError::StorageBackend("native B-tree value must be text".into())
        })?;
        visit(id, value)
    })?;
    private.advance()?;
    Ok(more)
}
