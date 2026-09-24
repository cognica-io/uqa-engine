//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate retained formats and migrate singleton checkpoints into atomic keyed records.

mod records;

use rusqlite::{params, Connection, OptionalExtension};
use uqa_storage::{
    mvcc::{DatabaseId, SerializableGraph, VersionError},
    read_control::StorageReadControl,
    StorageBackendError,
};

use super::super::PhysicalResult;
pub(super) use records::persist;

pub(super) const LEGACY_DEFINITION: &str = "CREATE TABLE _uqa_serializable_state (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), database_id BLOB NOT NULL CHECK(typeof(database_id) = 'blob' AND length(database_id) = 16), coordinator BLOB NOT NULL CHECK(typeof(coordinator) = 'blob' AND length(coordinator) = 16), checkpoint BLOB NOT NULL CHECK(typeof(checkpoint) = 'blob'))";
const DEFINITION: &str = "CREATE TABLE _uqa_serializable_state (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), database_id BLOB NOT NULL CHECK(typeof(database_id) = 'blob' AND length(database_id) = 16), coordinator BLOB NOT NULL CHECK(typeof(coordinator) = 'blob' AND length(coordinator) = 16))";
const RECORDS: &str = "CREATE TABLE _uqa_serializable_records (key BLOB PRIMARY KEY NOT NULL CHECK(typeof(key) = 'blob' AND length(key) = 49), value BLOB NOT NULL CHECK(typeof(value) = 'blob'))";
const APPLICATION_ID: i64 = 0x5551_5353;
const FORMAT: i64 = 2;

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
        if application != 0 || format != 0 {
            return Err(invalid().into());
        }
        let mut coordinator = [0; 16];
        getrandom::fill(&mut coordinator).map_err(|error| {
            VersionError::Storage(StorageBackendError::backend(
                "serializable identity",
                std::io::Error::other(error.to_string()),
            ))
        })?;
        initialize(connection, database, coordinator)?;
        return Ok(SerializableGraph::new(
            database,
            coordinator,
            control.memory(),
        )?);
    }
    let definition = match format {
        1 if count == 1 => LEGACY_DEFINITION,
        FORMAT if count == 2 => DEFINITION,
        _ => return Err(invalid().into()),
    };
    if application != APPLICATION_ID
        || !matches_definition(connection, "_uqa_serializable_state", definition)?
    {
        return Err(invalid().into());
    }
    let coordinator = identity(connection, database)?;
    if format == 1 {
        let mut blob =
            connection.blob_open("main", "_uqa_serializable_state", "checkpoint", 1, true)?;
        let graph = SerializableGraph::read_checkpoint(database, coordinator, &mut blob, control)?;
        blob.close()?;
        // Validation precedes migration. The admission transaction publishes the new schema and every record together, or restores the original checkpoint on any failure.
        connection.execute_batch("DROP TABLE _uqa_serializable_state")?;
        initialize(connection, database, coordinator)?;
        return Ok(graph);
    }
    if !matches_definition(connection, "_uqa_serializable_records", RECORDS)? {
        return Err(invalid().into());
    }
    records::load(connection, database, coordinator, control)
}

fn initialize(
    connection: &Connection,
    database: DatabaseId,
    coordinator: [u8; 16],
) -> PhysicalResult<()> {
    connection.pragma_update(None, "application_id", APPLICATION_ID)?;
    connection.pragma_update(None, "user_version", FORMAT)?;
    connection.execute_batch(DEFINITION)?;
    connection.execute_batch(RECORDS)?;
    connection.execute(
        "INSERT INTO _uqa_serializable_state VALUES (1, ?1, ?2)",
        params![database.as_bytes(), coordinator],
    )?;
    Ok(())
}

/// The caller has loaded and validated the old schema under physical SSI admission. Publish a fresh coordinator in that same transaction.
pub(super) fn restore(
    connection: &Connection,
    database: DatabaseId,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    let mut coordinator = [0; 16];
    getrandom::fill(&mut coordinator).map_err(|error| {
        VersionError::Storage(StorageBackendError::backend(
            "serializable restore identity",
            std::io::Error::other(error.to_string()),
        ))
    })?;
    let graph = SerializableGraph::new(database, coordinator, control.memory())?;
    connection.execute_batch(
        "DROP TABLE _uqa_serializable_records; DROP TABLE _uqa_serializable_state",
    )?;
    initialize(connection, database, coordinator)?;
    persist(connection, &graph, control)
}

fn matches_definition(
    connection: &Connection,
    name: &str,
    definition: &str,
) -> PhysicalResult<bool> {
    Ok(super::super::schema::definition_matches(connection, name, definition)? == Some(true))
}

fn identity(connection: &Connection, database: DatabaseId) -> PhysicalResult<[u8; 16]> {
    let identity: Option<([u8; 16], [u8; 16])> = connection
        .query_row(
            "SELECT database_id, coordinator FROM _uqa_serializable_state WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((stored, coordinator)) = identity else {
        return Err(invalid().into());
    };
    if stored != database.as_bytes() {
        return Err(VersionError::WrongDatabase.into());
    }
    Ok(coordinator)
}

fn invalid() -> VersionError {
    VersionError::InvalidEncoding("invalid serializable SQLite schema")
}
