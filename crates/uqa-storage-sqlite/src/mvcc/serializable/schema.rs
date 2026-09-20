//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate the complete auxiliary schema before decoding or creating shared SSI state.

use rusqlite::{params, Connection, OptionalExtension};
use uqa_storage::{
    mvcc::{DatabaseId, SerializableGraph, VersionError},
    read_control::StorageReadControl,
    StorageBackendError,
};

use super::super::PhysicalResult;

const DEFINITION: &str = "CREATE TABLE _uqa_serializable_state (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), database_id BLOB NOT NULL CHECK(typeof(database_id) = 'blob' AND length(database_id) = 16), coordinator BLOB NOT NULL CHECK(typeof(coordinator) = 'blob' AND length(coordinator) = 16), checkpoint BLOB NOT NULL CHECK(typeof(checkpoint) = 'blob'))";
const APPLICATION_ID: i64 = 0x5551_5353;
const FORMAT: i64 = 1;

pub(super) fn load(
    connection: &Connection,
    database: DatabaseId,
    control: &StorageReadControl,
) -> PhysicalResult<SerializableGraph> {
    control.check().map_err(VersionError::from)?;
    let application: i64 =
        connection.pragma_query_value(None, "application_id", |row| row.get(0))?;
    let format: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let count: i64 = connection.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*'",
        [],
        |row| row.get(0),
    )?;
    if count == 0 {
        // BEGIN IMMEDIATE can materialize an empty header page. Only our retained format markers distinguish initialized state from a fresh schema.
        if application != 0 || format != 0 {
            return Err(VersionError::InvalidEncoding("missing serializable SQLite schema").into());
        }
        connection.pragma_update(None, "application_id", APPLICATION_ID)?;
        connection.pragma_update(None, "user_version", FORMAT)?;
        connection.execute_batch(DEFINITION)?;
        let mut coordinator = [0; 16];
        getrandom::fill(&mut coordinator).map_err(|error| {
            VersionError::Storage(StorageBackendError::backend(
                "serializable identity",
                std::io::Error::other(error.to_string()),
            ))
        })?;
        let graph = SerializableGraph::new(database, coordinator, control.memory())?;
        connection.execute(
            "INSERT INTO _uqa_serializable_state VALUES (1, ?1, ?2, x'')",
            params![database.as_bytes(), coordinator],
        )?;
        return Ok(graph);
    }
    let definition = super::super::schema::definition_matches(
        connection,
        "_uqa_serializable_state",
        DEFINITION,
    )?;
    if application != APPLICATION_ID || format != FORMAT || count != 1 || definition != Some(true) {
        return Err(VersionError::InvalidEncoding("invalid serializable SQLite schema").into());
    }
    let identity: Option<([u8; 16], [u8; 16])> = connection
        .query_row(
            "SELECT database_id, coordinator FROM _uqa_serializable_state WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((stored_database, coordinator)) = identity else {
        return Err(VersionError::InvalidEncoding("missing serializable SQLite state").into());
    };
    if stored_database != database.as_bytes() {
        return Err(VersionError::WrongDatabase.into());
    }
    let mut blob =
        connection.blob_open("main", "_uqa_serializable_state", "checkpoint", 1, true)?;
    let graph = SerializableGraph::read_checkpoint(database, coordinator, &mut blob, control)?;
    blob.close()?;
    Ok(graph)
}
