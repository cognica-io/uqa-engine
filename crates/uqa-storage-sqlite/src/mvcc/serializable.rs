//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Atomic, encrypted transport for the common SSI graph across independent `SQLite` processes.

mod liveness;
mod schema;
#[cfg(test)]
mod tests;

use rusqlite::Connection;
use uqa_storage::{
    mvcc::{SerializableGraph, VersionError, VersionResult},
    read_control::StorageReadControl,
};

use super::{admission, Error, PhysicalResult, SQLiteRecordStore};
use crate::SQLiteConnectionLease;

/// An exclusive, short-lived SSI admission over the database's auxiliary store. Every file-backed opener uses `SQLite`'s process-shared writer admission; in-memory sessions share their parent's retained auxiliary pool. Dropping this guard discards unpersisted changes and releases admission.
///
/// The caller owns participant liveness, receipt reconciliation and snapshot/publication ordering. Persist admission before exposing a participant, and persist prepared receipt bindings before publishing records, then reacquire admission while publishing and resolving them. A failed persistence call does not establish a physical record outcome. This transport does not enable SERIALIZABLE SQL by itself.
pub struct SQLiteSerializableAdmission {
    connection: SQLiteConnectionLease,
    graph: SerializableGraph,
    previous_timeout: std::time::Duration,
}

impl SQLiteRecordStore {
    /// Acquire the shared SSI state without retaining a main-database physical transaction. The supplied allowance bounds the decoded graph and its predicate keys; serialization streams directly to a transactional BLOB.
    pub fn serializable_admission(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<SQLiteSerializableAdmission> {
        control.check()?;
        let auxiliary = self
            .connection
            .serializable_connection(self.identity, control)?;
        let connection = auxiliary
            .lease_connection_with_control(control)
            .map_err(|error| Error::from(error).into_version())?;
        let timeout = admission::BusyTimeout::new(&connection).map_err(super::sqlite_error)?;
        begin(&connection, control).map_err(Error::into_version)?;
        let graph =
            schema::load(&connection, self.identity, control).map_err(Error::into_version)?;
        drop(timeout);
        let milliseconds: u32 = connection
            .pragma_query_value(None, "busy_timeout", |row| row.get(0))
            .map_err(super::sqlite_error)?;
        connection
            .busy_timeout(std::time::Duration::ZERO)
            .map_err(super::sqlite_error)?;
        Ok(SQLiteSerializableAdmission {
            connection,
            graph,
            previous_timeout: std::time::Duration::from_millis(u64::from(milliseconds)),
        })
    }
}

impl SQLiteSerializableAdmission {
    pub fn graph(&self) -> &SerializableGraph {
        &self.graph
    }

    pub fn graph_mut(&mut self) -> &mut SerializableGraph {
        &mut self.graph
    }

    /// Atomically retain changes and release admission. An unchanged checkpoint needs no BLOB replacement. Cancellation or a failed BLOB write rolls back the auxiliary transaction. A failed COMMIT may be uncertain; callers must reconcile physical receipts before classifying a publication or admitting another data snapshot.
    pub fn persist(self, control: &StorageReadControl) -> VersionResult<()> {
        self.persist_in(control).map_err(Error::into_version)
    }

    fn persist_in(&self, control: &StorageReadControl) -> PhysicalResult<()> {
        if !self.graph.checkpoint_changed() {
            return retry(&self.connection, "COMMIT", false, control);
        }
        let length = i32::try_from(self.graph.checkpoint_length(control)?).map_err(|_| {
            VersionError::InvalidEncoding("serializable checkpoint exceeds SQLite BLOB capacity")
        })?;
        self.connection.execute(
            "UPDATE _uqa_serializable_state SET checkpoint = zeroblob(?1) WHERE singleton = 1",
            [length],
        )?;
        let mut blob =
            self.connection
                .blob_open("main", "_uqa_serializable_state", "checkpoint", 1, false)?;
        self.graph.write_checkpoint(&mut blob, control)?;
        blob.close()?;
        retry(&self.connection, "COMMIT", false, control)
    }
}

impl Drop for SQLiteSerializableAdmission {
    fn drop(&mut self) {
        // The owned lease performs rollback before returning the connection to its pool.
        let _ = self.connection.busy_timeout(self.previous_timeout);
    }
}

fn begin(connection: &Connection, control: &StorageReadControl) -> PhysicalResult<()> {
    retry(connection, "BEGIN IMMEDIATE", true, control)
}

fn retry(
    connection: &Connection,
    sql: &str,
    autocommit: bool,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    admission::retry(connection, autocommit, control, || {
        connection.execute_batch(sql).map_err(Into::into)
    })
}
