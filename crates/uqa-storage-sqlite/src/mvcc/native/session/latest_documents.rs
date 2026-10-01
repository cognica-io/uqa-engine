//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Committed document rows read from their physical projection when it holds exactly a snapshot's records.
//!
//! Each record commit materializes the native projection in its own physical transaction and advances the database header in that transaction, so a read whose header sequence equals a snapshot's boundary sees the projection of exactly that boundary. Commit validation binds every document row's table name to its owner, and a name is retired only after its rows are gone, so the rows under a table's name are the records of the owner bound to that name. When that owner has no other bound name and the session holds no private records, the physical rows in document order stand in for the record scan, one sequential cursor instead of a version lookup per record.

use rusqlite::{params, types::ValueRef, Connection, OptionalExtension};
use uqa_core::memory::{MemoryError, MemoryReservation};
use uqa_storage::mvcc::VersionError;
use uqa_storage::read_control::StorageReadControl;

use super::{owners, NativeRecordOwner, NativeSnapshot};
use crate::connection::Result;
use crate::mvcc::PhysicalResult;
use crate::read_control::{payload_length, reserve_bindings};

/// Bodies of at most this many bytes are read with their row. The selected byte length bounds `SQLite`'s copy before the body is evaluated; a larger body is admitted and then read by itself.
const INLINE_BODY_BYTES: u16 = 16 * 1024;

const ROWS: &str = "SELECT doc_id, octet_length(body), CASE WHEN octet_length(body) <= ?2 THEN body END, tuple_xmin FROM _documents WHERE table_name = ?1 ORDER BY doc_id";
const ROWS_AFTER: &str = "SELECT doc_id, octet_length(body), CASE WHEN octet_length(body) <= ?2 THEN body END, tuple_xmin FROM _documents WHERE table_name = ?1 AND doc_id > ?3 ORDER BY doc_id";
const BODY: &str = "SELECT body FROM _documents WHERE table_name = ?1 AND doc_id = ?2";
const NAMES_OF_OWNER: &str = "SELECT count(*) FROM _uqa_mvcc_native_owners WHERE (object_id, generation) = (SELECT object_id, generation FROM _uqa_mvcc_native_owners WHERE name = ?1)";

/// The table's data generation, which the `_documents` triggers advance in the transaction of every row change.
const GENERATION: &str =
    "SELECT generation FROM _cache_revisions WHERE kind = 'data' AND name = ?1";

/// One physical read of a table's latest committed rows.
pub(crate) struct LatestDocuments<'a> {
    connection: &'a Connection,
    table: &'a str,
    generation: i64,
    control: &'a StorageReadControl,
}

impl LatestDocuments<'_> {
    /// Every commit that changes the table's rows advances this generation in the same transaction, so reads observing an equal generation for the same owner see the same rows.
    pub(crate) fn generation(&self) -> i64 {
        self.generation
    }

    /// Visit the rows after document `after`, in document order and in `_documents` column order, while `visit` returns true. `visit` runs inside the physical read and must not read the snapshot.
    pub(crate) fn visit(
        &self,
        after: Option<i64>,
        visit: &mut dyn FnMut(&[ValueRef<'_>]) -> Result<bool>,
    ) -> Result<()> {
        visit_rows(self.connection, self.table, after, self.control, visit)
            .map_err(|error| error.into_version().into())
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
        if self.view.private_revision().is_some() {
            return Ok(None);
        }
        let Some(snapshot) = self
            .view
            .committed()
            .provider_snapshot()
            .and_then(|snapshot| snapshot.downcast_ref::<crate::mvcc::read::Snapshot>())
        else {
            return Ok(None);
        };
        let result = snapshot.read_latest(|connection| {
            if owners::lookup(connection, ValueRef::Text(table.as_bytes()), control)? != Some(owner)
            {
                return Ok(None);
            }
            let names: i64 = connection
                .prepare_cached(NAMES_OF_OWNER)?
                .query_row([table], |row| row.get(0))?;
            if names != 1 {
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
            };
            Ok(Some(read(&latest)?))
        })?;
        self.control.check()?;
        control.check()?;
        Ok(result)
    }
}

fn visit_rows(
    connection: &Connection,
    table: &str,
    after: Option<i64>,
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
    while let Some(row) = rows.next()? {
        control.check().map_err(VersionError::from)?;
        let id: i64 = row.get(0)?;
        let length = payload_length(row.get(1)?)?;
        admitted = Some(
            control
                .memory()
                .reserve(
                    length
                        .checked_add(table.len())
                        .ok_or(MemoryError::SizeOverflow)
                        .map_err(VersionError::from)?,
                )
                .map_err(VersionError::from)?,
        );
        let xmin = row.get_ref(3)?;
        let more = match row.get_ref(2)? {
            ValueRef::Text(body) if body.len() == length => visit(&[
                ValueRef::Text(table.as_bytes()),
                ValueRef::Integer(id),
                ValueRef::Text(body),
                xmin,
            ])?,
            ValueRef::Null if length > usize::from(INLINE_BODY_BYTES) => {
                let mut body_rows = body_statement.query(params![table, id])?;
                let body_row = body_rows.next()?.ok_or(VersionError::InvalidEncoding(
                    "native document disappeared within a read",
                ))?;
                match body_row.get_ref(0)? {
                    ValueRef::Text(body) if body.len() == length => visit(&[
                        ValueRef::Text(table.as_bytes()),
                        ValueRef::Integer(id),
                        ValueRef::Text(body),
                        xmin,
                    ])?,
                    _ => {
                        return Err(VersionError::InvalidEncoding(
                            "native document body changed within a read",
                        )
                        .into())
                    }
                }
            }
            _ => {
                return Err(
                    VersionError::InvalidEncoding("native document body must be text").into(),
                )
            }
        };
        if !more {
            break;
        }
    }
    drop(rows);
    drop(statement);
    drop(body_statement);
    drop(admitted);
    control.check().map_err(VersionError::from)?;
    Ok(())
}
