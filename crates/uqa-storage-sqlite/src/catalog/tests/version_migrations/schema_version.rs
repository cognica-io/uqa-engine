//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn corrupt_schema_version_is_reported_instead_of_replaying_migrations() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let _current = Catalog::open(connection.clone()).unwrap();
    connection
        .with(|database| {
            database.execute(
                "UPDATE _metadata SET value = 'not-a-version' WHERE key = 'schema_version'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let error = Catalog::open(connection).err();
    assert!(matches!(
        error,
        Some(SQLiteError::InvalidSchemaVersion(version)) if version == "not-a-version"
    ));
}

#[test]
fn future_schema_version_is_rejected() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let _current = Catalog::open(connection.clone()).unwrap();
    let future = CURRENT_SCHEMA_VERSION + 1;
    connection
        .with(|database| {
            database.execute(
                "UPDATE _metadata SET value = ?1 WHERE key = 'schema_version'",
                [future.to_string()],
            )?;
            Ok(())
        })
        .unwrap();

    assert!(matches!(
        Catalog::open(connection).err(),
        Some(SQLiteError::UnsupportedSchemaVersion { found, supported })
            if found == future && supported == CURRENT_SCHEMA_VERSION
    ));
}

#[test]
fn native_catalog_marker_is_not_replayed_as_a_physical_migration() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    catalog
        .save_foreign_server("preserved", "memory", "opaque options")
        .unwrap();
    let before = connection.with(|database| {
        database.execute_batch("DROP TABLE _foreign_server_metadata; UPDATE _metadata SET value='49' WHERE key='schema_version'")?;
        Ok(database.pragma_query_value(None, "schema_version", |row| row.get::<_, i64>(0))?)
    }).unwrap();
    assert!(matches!(
        Catalog::open(connection.clone()),
        Err(SQLiteError::LogicalSessionRequired)
    ));
    connection
        .with(|database| {
            assert_eq!(
                database.pragma_query_value(None, "schema_version", |row| row.get::<_, i64>(0))?,
                before
            );
            assert!(!table_exists(database, "_foreign_server_metadata")?);
            assert_eq!(
                database.query_row(
                    "SELECT value FROM _metadata WHERE key='schema_version'",
                    [],
                    |row| row.get::<_, String>(0)
                )?,
                "49"
            );
            assert_eq!(
                database.query_row(
                    "SELECT options FROM _foreign_servers WHERE name='preserved'",
                    [],
                    |row| row.get::<_, String>(0)
                )?,
                "opaque options"
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn native_catalog_open_requires_binding_even_through_record_access() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    connection
        .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions::default())
        .unwrap();
    let before = connection
        .with_physical(|database| {
            Ok(database.pragma_query_value(None, "schema_version", |row| row.get::<_, i64>(0))?)
        })
        .unwrap();
    // Internal physical access does not confer permission to run physical catalog migrations on a mapped catalog.
    assert!(matches!(
        Catalog::open(connection.record_connection()),
        Err(SQLiteError::LogicalSessionRequired)
    ));
    Catalog::open(connection.clone()).unwrap();
    connection
        .with_physical(|database| {
            assert_eq!(
                database.pragma_query_value(None, "schema_version", |row| row.get::<_, i64>(0))?,
                before
            );
            assert_eq!(
                database.query_row(
                    "SELECT value FROM _metadata WHERE key='schema_version'",
                    [],
                    |row| row.get::<_, String>(0)
                )?,
                "49"
            );
            Ok(())
        })
        .unwrap();
}
