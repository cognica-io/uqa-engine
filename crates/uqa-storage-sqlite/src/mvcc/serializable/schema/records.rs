//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stream only changed common records through transactional `SQLite` BLOBs.

use rusqlite::{params, Connection};
use uqa_storage::{
    mvcc::{DatabaseId, SerializableGraph, VersionError},
    read_control::StorageReadControl,
};

use super::super::super::{sqlite_error, PhysicalResult};

pub(super) fn load(
    connection: &Connection,
    database: DatabaseId,
    coordinator: [u8; 16],
    control: &StorageReadControl,
) -> PhysicalResult<SerializableGraph> {
    Ok(SerializableGraph::read_checkpoint_records(
        database,
        coordinator,
        control,
        |visit| {
            let mut statement = connection
                .prepare("SELECT rowid, key FROM _uqa_serializable_records ORDER BY key")
                .map_err(sqlite_error)?;
            let mut rows = statement.query([]).map_err(sqlite_error)?;
            while let Some(row) = rows.next().map_err(sqlite_error)? {
                control.check()?;
                let rowid: i64 = row.get(0).map_err(sqlite_error)?;
                let key: [u8; 49] = row.get(1).map_err(sqlite_error)?;
                let mut blob = connection
                    .blob_open("main", "_uqa_serializable_records", "value", rowid, true)
                    .map_err(sqlite_error)?;
                visit(&key, &mut blob)?;
                blob.close().map_err(sqlite_error)?;
            }
            Ok(())
        },
    )?)
}

pub(in crate::mvcc::serializable) fn persist(
    connection: &Connection,
    graph: &SerializableGraph,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    graph.write_checkpoint_changes(control, |key, record| {
        let bytes = key.as_bytes().as_slice();
        let Some(record) = record else {
            if connection.execute("DELETE FROM _uqa_serializable_records WHERE key = ?1", [bytes]).map_err(sqlite_error)? != 1 {
                return Err(VersionError::InvalidEncoding("missing serializable checkpoint record"));
            }
            return Ok(());
        };
        let length = i32::try_from(record.encoded_length(control)?).map_err(|_| VersionError::InvalidEncoding("serializable record exceeds SQLite BLOB capacity"))?;
        connection.execute("INSERT INTO _uqa_serializable_records(key, value) VALUES (?1, zeroblob(?2)) ON CONFLICT(key) DO UPDATE SET value = excluded.value", params![bytes, length]).map_err(sqlite_error)?;
        let rowid: i64 = connection.query_row("SELECT rowid FROM _uqa_serializable_records WHERE key = ?1", [bytes], |row| row.get(0)).map_err(sqlite_error)?;
        let mut blob = connection.blob_open("main", "_uqa_serializable_records", "value", rowid, false).map_err(sqlite_error)?;
        record.write(&mut blob, control)?;
        blob.close().map_err(sqlite_error)?;
        Ok(())
    })?;
    Ok(())
}
