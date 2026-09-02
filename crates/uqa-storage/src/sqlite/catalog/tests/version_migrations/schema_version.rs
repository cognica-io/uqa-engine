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
