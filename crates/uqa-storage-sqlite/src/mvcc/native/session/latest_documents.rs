//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Committed document rows read from their physical projection when it holds exactly a snapshot's records.
//!
//! At a snapshot that is the latest commit, the projection holds the snapshot's committed records (see [`NativeSnapshot::read_latest_projection`]). Commit validation binds every document row's table name to its owner, and a name is retired only after its rows are gone, so the rows under a table's name are the records of the owner bound to that name. When that owner has no other bound name, the physical rows in document order stand in for the record scan, one sequential cursor instead of a version lookup per record. Private records of other tables are not among these rows. The session's private document records of the owner, which the projection does not hold, are merged into them in document order: a private record before a row inserts a document, one at a row replaces or deletes it, and the work is that of the private records rather than a version lookup for every row.

use rusqlite::{params, types::ValueRef, Connection, OptionalExtension, Row, Statement};
use uqa_core::memory::{MemoryError, MemoryReservation};
use uqa_storage::mvcc::{MergedRecordSnapshot, VersionError};
use uqa_storage::read_control::StorageReadControl;

use super::private_rows::PrivateRows;
use super::{owners, Family, NativeRecordIdentity, NativeRecordOwner, NativeSnapshot};
use crate::connection::Result;
use crate::mvcc::PhysicalResult;
use crate::read_control::{payload_length, reserve_bindings};

mod historical;
mod points;

/// Bodies of at most this many bytes are read with their row. The selected byte length bounds `SQLite`'s copy before the body is evaluated; a larger body is admitted and then read by itself.
const INLINE_BODY_BYTES: u16 = 16 * 1024;

const ROWS: &str = "SELECT doc_id, octet_length(body), CASE WHEN octet_length(body) <= ?2 THEN body END, tuple_xmin FROM _documents WHERE table_name = ?1 ORDER BY doc_id";
const ROWS_AFTER: &str = "SELECT doc_id, octet_length(body), CASE WHEN octet_length(body) <= ?2 THEN body END, tuple_xmin FROM _documents WHERE table_name = ?1 AND doc_id > ?3 ORDER BY doc_id";
const BODY: &str = "SELECT body FROM _documents WHERE table_name = ?1 AND doc_id = ?2";
const COUNT: &str = "SELECT count(*) FROM _documents WHERE table_name = ?1";
const IDS: &str = "SELECT doc_id FROM _documents WHERE table_name = ?1 ORDER BY doc_id";
const IDS_AFTER: &str =
    "SELECT doc_id FROM _documents WHERE table_name = ?1 AND doc_id > ?2 ORDER BY doc_id";
const STORED: &str =
    "SELECT EXISTS(SELECT 1 FROM _documents WHERE table_name = ?1 AND doc_id = ?2)";
const NAMES_OF_OWNER: &str = "SELECT count(*) FROM _uqa_mvcc_native_owners WHERE (object_id, generation) = (SELECT object_id, generation FROM _uqa_mvcc_native_owners WHERE name = ?1)";

/// The table's data generation, which the `_documents` triggers advance in the transaction of every row change.
const GENERATION: &str =
    "SELECT generation FROM _cache_revisions WHERE kind = 'data' AND name = ?1";

/// One physical read of a table's latest committed rows, with the session's private records of them.
pub(crate) struct LatestDocuments<'a> {
    connection: &'a Connection,
    table: &'a str,
    generation: i64,
    control: &'a StorageReadControl,
    view: &'a MergedRecordSnapshot,
    documents: NativeRecordIdentity,
    /// Whether the session holds a private record under the document key prefix of the table's owner.
    private: bool,
}

impl LatestDocuments<'_> {
    /// The data generation of the table's stored rows. Every commit that changes them advances it in the same transaction, so reads observing an equal generation for the same owner see the same stored rows; the session's private records, which [`Self::private_documents`] yields, are not among them.
    pub(crate) fn generation(&self) -> i64 {
        self.generation
    }

    /// Visit the rows after document `after`, in document order and in `_documents` column order, while `visit` returns true. `visit` runs inside the physical read and must not read the snapshot.
    pub(crate) fn visit(
        &self,
        after: Option<i64>,
        visit: &mut dyn FnMut(&[ValueRef<'_>]) -> Result<bool>,
    ) -> Result<()> {
        let private = self.private_documents(after)?;
        visit_rows(
            self.connection,
            self.table,
            after,
            private,
            self.control,
            visit,
        )
        .map_err(|error| error.into_version().into())
    }

    /// Visit the identities of the rows after document `after`, in document order, while `visit` returns true, without reading a body. `visit` runs inside the physical read and must not read the snapshot.
    pub(crate) fn visit_ids(
        &self,
        after: Option<i64>,
        visit: &mut dyn FnMut(i64) -> Result<bool>,
    ) -> Result<()> {
        let private = self.private_documents(after)?;
        visit_ids(
            self.connection,
            self.table,
            after,
            private,
            self.control,
            visit,
        )
        .map_err(|error| error.into_version().into())
    }

    /// The number of rows, counted in the table's key order without reading a body, with the documents the session's private records insert or delete.
    pub(crate) fn count(&self) -> Result<u64> {
        let private = self.private_documents(None)?;
        count_rows(self.connection, self.table, private, self.control)
            .map_err(|error| error.into_version().into())
    }

    /// The session's private document records of the table after document `after`, in document order, or `None` when it holds none. The rows [`Self::visit`] yields have them merged in already.
    pub(crate) fn private_documents(&self, after: Option<i64>) -> Result<Option<PrivateRows<'_>>> {
        Ok(self
            .private
            .then(|| PrivateRows::after(self.view, self.documents, &[], after, self.control))
            .transpose()?)
    }
}

impl NativeSnapshot {
    /// Run `read` over the latest committed rows of `table` in one physical read. Returns `None` without running it when the physical rows cannot stand in for this snapshot's records of `owner`; the caller then reads the records.
    pub(crate) fn read_latest_documents<T>(
        &self,
        table: &str,
        owner: NativeRecordOwner,
        control: &StorageReadControl,
        read: &mut dyn FnMut(&LatestDocuments<'_>) -> Result<T>,
    ) -> Result<Option<T>> {
        self.control.check()?;
        control.check()?;
        let documents = NativeRecordIdentity::new(Family::Documents, owner)?;
        let prefix = documents.encode_prefix(&[], control)?;
        // A session that holds no private record at all skips the seek for one under the prefix.
        let private = self.view.private_revision().is_some()
            && !self
                .view
                .private_keys(&prefix, None, 1, control)?
                .is_empty();
        self.read_latest_committed(control, &mut |connection| {
            if !stores_alone(connection, table, owner, control)? {
                return Ok(None);
            }
            let generation = connection
                .prepare_cached(GENERATION)?
                .query_row([table], |row| row.get(0))
                .optional()?
                .unwrap_or(0);
            let latest = LatestDocuments {
                connection,
                table,
                generation,
                control,
                view: &self.view,
                documents,
                private,
            };
            read(&latest).map(Some)
        })
    }
}

/// Whether the stored rows under `table`'s name are those of `owner` alone: the committed binding of the name is `owner`, and no other name is bound to it.
pub(super) fn stores_alone(
    connection: &Connection,
    table: &str,
    owner: NativeRecordOwner,
    control: &StorageReadControl,
) -> Result<bool> {
    let bound = owners::lookup(connection, ValueRef::Text(table.as_bytes()), control)
        .map_err(crate::mvcc::Error::into_version)?;
    if bound != Some(owner) {
        return Ok(false);
    }
    let names: i64 = connection
        .prepare_cached(NAMES_OF_OWNER)?
        .query_row([table], |row| row.get(0))?;
    Ok(names == 1)
}

fn count_rows(
    connection: &Connection,
    table: &str,
    private: Option<PrivateRows<'_>>,
    control: &StorageReadControl,
) -> PhysicalResult<u64> {
    let _bindings = reserve_bindings(control, &[table.as_bytes()])?;
    let count: i64 = connection
        .prepare_cached(COUNT)?
        .query_row([table], |row| row.get(0))?;
    control.check().map_err(VersionError::from)?;
    let mut count = u64::try_from(count)
        .map_err(|_| VersionError::InvalidEncoding("negative native document count"))?;
    if let Some(mut private) = private {
        let mut stored = connection.prepare_cached(STORED)?;
        while let Some(id) = private.peek() {
            control.check().map_err(VersionError::from)?;
            let present: bool = stored.query_row(params![table, id], |row| row.get(0))?;
            match (private.live()?, present) {
                (true, false) => count += 1,
                (false, true) => {
                    count = count.checked_sub(1).ok_or(VersionError::InvalidEncoding(
                        "a private deletion exceeds the native document count",
                    ))?;
                }
                _ => {}
            }
            private.advance()?;
        }
    }
    Ok(count)
}

fn visit_rows(
    connection: &Connection,
    table: &str,
    after: Option<i64>,
    mut private: Option<PrivateRows<'_>>,
    control: &StorageReadControl,
    visit: &mut dyn FnMut(&[ValueRef<'_>]) -> Result<bool>,
) -> PhysicalResult<()> {
    let _bindings = reserve_bindings(control, &[table.as_bytes()])?;
    let limit = i64::from(INLINE_BODY_BYTES);
    // Declared before the statements so that every exit finalizes them first: a row's allowance is released only after stepping or finalization releases its SQLite-owned buffers.
    let mut admitted: Option<MemoryReservation> = None;
    let mut body_statement = connection.prepare_cached(BODY)?;
    let mut statement =
        connection.prepare_cached(if after.is_some() { ROWS_AFTER } else { ROWS })?;
    let mut rows = match after {
        Some(after) => statement.query(params![table, limit, after])?,
        None => statement.query(params![table, limit])?,
    };
    let mut stopped = false;
    while let Some(row) = rows.next()? {
        control.check().map_err(VersionError::from)?;
        let id: i64 = row.get(0)?;
        // A private record before this row inserts a document; one at this row replaces or deletes it.
        let mut replaced = false;
        if let Some(private) = private.as_mut() {
            while let Some(next) = private.peek().filter(|next| *next <= id) {
                let more = private.visit(visit)?;
                private.advance()?;
                replaced |= next == id;
                if more == Some(false) {
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
        admitted = Some(admit_row(row, table, control)?);
        let more = visit_stored_row(row, table, &mut body_statement, visit)?;
        if !more {
            stopped = true;
            break;
        }
    }
    drop(rows);
    drop(statement);
    drop(body_statement);
    drop(admitted);
    // The private records after the last row insert documents.
    if let Some(private) = private.as_mut().filter(|_| !stopped) {
        while private.peek().is_some() {
            control.check().map_err(VersionError::from)?;
            let more = private.visit(visit)?;
            private.advance()?;
            if more == Some(false) {
                break;
            }
        }
    }
    control.check().map_err(VersionError::from)?;
    Ok(())
}

fn visit_ids(
    connection: &Connection,
    table: &str,
    after: Option<i64>,
    mut private: Option<PrivateRows<'_>>,
    control: &StorageReadControl,
    visit: &mut dyn FnMut(i64) -> Result<bool>,
) -> PhysicalResult<()> {
    let _bindings = reserve_bindings(control, &[table.as_bytes()])?;
    let mut statement = connection.prepare_cached(if after.is_some() { IDS_AFTER } else { IDS })?;
    let mut rows = match after {
        Some(after) => statement.query(params![table, after])?,
        None => statement.query(params![table])?,
    };
    let mut stopped = false;
    while let Some(row) = rows.next()? {
        control.check().map_err(VersionError::from)?;
        let id: i64 = row.get(0)?;
        // A private record before this row inserts a document; one at this row replaces or deletes it.
        let mut replaced = false;
        if let Some(private) = private.as_mut() {
            while let Some(next) = private.peek().filter(|next| *next <= id) {
                let live = private.live()?;
                private.advance()?;
                replaced |= next == id;
                if live && !visit(next)? {
                    stopped = true;
                    break;
                }
            }
        }
        if stopped {
            break;
        }
        if !replaced && !visit(id)? {
            stopped = true;
            break;
        }
    }
    drop(rows);
    drop(statement);
    // The private records after the last row insert documents.
    if let Some(private) = private.as_mut().filter(|_| !stopped) {
        while let Some(next) = private.peek() {
            control.check().map_err(VersionError::from)?;
            let live = private.live()?;
            private.advance()?;
            if live && !visit(next)? {
                break;
            }
        }
    }
    control.check().map_err(VersionError::from)?;
    Ok(())
}

fn admit_row(
    row: &Row<'_>,
    table: &str,
    control: &StorageReadControl,
) -> PhysicalResult<MemoryReservation> {
    let length = payload_length(row.get(1)?)?;
    Ok(control
        .memory()
        .reserve(
            length
                .checked_add(table.len())
                .ok_or(MemoryError::SizeOverflow)
                .map_err(VersionError::from)?,
        )
        .map_err(VersionError::from)?)
}

fn visit_stored_row(
    row: &Row<'_>,
    table: &str,
    body_statement: &mut Statement<'_>,
    visit: &mut dyn FnMut(&[ValueRef<'_>]) -> Result<bool>,
) -> PhysicalResult<bool> {
    let id: i64 = row.get(0)?;
    let length = payload_length(row.get(1)?)?;
    let xmin = row.get_ref(3)?;
    match row.get_ref(2)? {
        ValueRef::Text(body) if body.len() == length => Ok(visit(&[
            ValueRef::Text(table.as_bytes()),
            ValueRef::Integer(id),
            ValueRef::Text(body),
            xmin,
        ])?),
        ValueRef::Null if length > usize::from(INLINE_BODY_BYTES) => {
            let mut body_rows = body_statement.query(params![table, id])?;
            let body_row = body_rows.next()?.ok_or(VersionError::InvalidEncoding(
                "native document disappeared within a read",
            ))?;
            match body_row.get_ref(0)? {
                ValueRef::Text(body) if body.len() == length => Ok(visit(&[
                    ValueRef::Text(table.as_bytes()),
                    ValueRef::Integer(id),
                    ValueRef::Text(body),
                    xmin,
                ])?),
                _ => Err(VersionError::InvalidEncoding(
                    "native document body changed within a read",
                )
                .into()),
            }
        }
        _ => Err(VersionError::InvalidEncoding("native document body must be text").into()),
    }
}
