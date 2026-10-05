//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::mvcc::VersionedSessionOptions;

fn server(metadata: Option<&str>) -> ForeignServerRow {
    ForeignServerRow {
        name: "remote".into(),
        fdw_type: "memory".into(),
        options_json: "opaque options".into(),
        metadata_json: metadata.map(str::to_owned),
    }
}

#[test]
fn foreign_server_metadata_round_trips_undo_and_reopens_in_both_catalog_modes() {
    for native in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("servers.sqlite3");
        {
            let connection = ManagedConnection::open(&path).unwrap();
            let catalog = Catalog::open(connection.clone()).unwrap();
            catalog
                .save_foreign_server("remote", "memory", "opaque options")
                .unwrap();
            if native {
                connection
                    .bind_native_records(VersionedSessionOptions::default())
                    .unwrap();
            }
            assert_eq!(catalog.load_foreign_server_rows().unwrap(), [server(None)]);
            catalog
                .save_foreign_server_row(&server(Some("original")))
                .unwrap();
            connection.begin_transaction().unwrap();
            connection.savepoint("server").unwrap();
            catalog
                .save_foreign_server("remote", "replacement", "changed options")
                .unwrap();
            let updated = catalog.load_foreign_server_rows().unwrap().remove(0);
            assert_eq!(updated.fdw_type, "replacement");
            assert_eq!(updated.metadata_json.as_deref(), Some("original"));
            catalog
                .save_foreign_server_row(&server(Some("private")))
                .unwrap();
            catalog.drop_foreign_server("remote").unwrap();
            assert!(catalog.load_foreign_server_rows().unwrap().is_empty());
            connection.rollback_to_savepoint("server").unwrap();
            assert_eq!(
                catalog.load_foreign_server_rows().unwrap(),
                [server(Some("original"))]
            );
            connection.commit_transaction().unwrap();
            catalog.drop_foreign_server("remote").unwrap();
            catalog
                .save_foreign_server("remote", "memory", "opaque options")
                .unwrap();
            assert_eq!(catalog.load_foreign_server_rows().unwrap(), [server(None)]);
            catalog
                .save_foreign_server_row(&server(Some("final")))
                .unwrap();
        }
        let connection = ManagedConnection::open(&path).unwrap();
        if native {
            connection
                .bind_native_records(VersionedSessionOptions::default())
                .unwrap();
        }
        let catalog = Catalog::open(connection).unwrap();
        assert_eq!(
            catalog.load_foreign_server_rows().unwrap(),
            [server(Some("final"))]
        );
    }
}

#[test]
fn native_server_metadata_and_body_share_retained_snapshots() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    let writer = Catalog::open(connection.clone()).unwrap();
    writer
        .save_foreign_server_row(&server(Some("first")))
        .unwrap();
    let reader = connection.new_session();
    let catalog = Catalog::open(reader.clone()).unwrap();
    reader.begin_record_read().unwrap();
    assert_eq!(
        catalog.load_foreign_server_rows().unwrap(),
        [server(Some("first"))]
    );
    let mut second = server(Some("second"));
    second.fdw_type = "changed".into();
    second.options_json = "changed options".into();
    writer.save_foreign_server_row(&second).unwrap();
    assert_eq!(
        catalog.load_foreign_server_rows().unwrap(),
        [server(Some("first"))]
    );
    reader.commit_transaction().unwrap();
    assert_eq!(
        catalog.load_foreign_server_rows().unwrap(),
        [second.clone()]
    );
    reader.begin_record_read().unwrap();
    writer.drop_foreign_server("remote").unwrap();
    assert_eq!(catalog.load_foreign_server_rows().unwrap(), [second]);
    reader.commit_transaction().unwrap();
    assert!(catalog.load_foreign_server_rows().unwrap().is_empty());
}

#[test]
fn server_metadata_rejects_orphans_in_both_catalog_modes() {
    for native in [false, true] {
        let connection = ManagedConnection::open_in_memory().unwrap();
        let catalog = Catalog::open(connection.clone()).unwrap();
        if native {
            connection
                .bind_native_records(VersionedSessionOptions::default())
                .unwrap();
            connection
                .with_native_write(|snapshot, batch| {
                    snapshot.put_row(
                        batch,
                        crate::mvcc::native::NativeRecordFamily::ForeignServerMetadata,
                        crate::mvcc::native::NativeRecordOwner::Database(snapshot.database),
                        &[
                            rusqlite::types::ValueRef::Text(b"absent"),
                            rusqlite::types::ValueRef::Text(b"metadata"),
                        ],
                    )
                })
                .unwrap();
        } else {
            connection
                .with(|sqlite| {
                    sqlite.execute(
                        "INSERT INTO _foreign_server_metadata VALUES ('absent', 'metadata')",
                        [],
                    )?;
                    Ok(())
                })
                .unwrap();
        }
        let error = catalog.load_foreign_server_rows().unwrap_err();
        assert!(
            error
                .to_string()
                .contains("metadata references missing server `absent`"),
            "{error}"
        );
    }
}

#[test]
fn physical_server_row_write_and_delete_are_atomic_on_metadata_failure() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    connection.with(|sqlite| {
        sqlite.execute_batch("CREATE TRIGGER reject_metadata BEFORE INSERT ON _foreign_server_metadata BEGIN SELECT RAISE(ABORT, 'reject metadata'); END;")?;
        Ok(())
    }).unwrap();
    assert!(catalog
        .save_foreign_server_row(&server(Some("identity")))
        .is_err());
    assert!(catalog.load_foreign_server_rows().unwrap().is_empty());
    connection
        .with(|sqlite| {
            sqlite.execute_batch("DROP TRIGGER reject_metadata")?;
            Ok(())
        })
        .unwrap();
    catalog
        .save_foreign_server_row(&server(Some("identity")))
        .unwrap();
    connection.with(|sqlite| {
        sqlite.execute_batch("CREATE TRIGGER reject_body_delete BEFORE DELETE ON _foreign_servers BEGIN SELECT RAISE(ABORT, 'reject delete'); END;")?;
        Ok(())
    }).unwrap();
    assert!(catalog.drop_foreign_server("remote").is_err());
    assert_eq!(
        catalog.load_foreign_server_rows().unwrap(),
        [server(Some("identity"))]
    );
}

#[test]
fn legacy_server_metadata_migration_is_atomic_and_does_not_create_sql_identity() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    catalog
        .save_foreign_server("remote", "memory", "opaque options")
        .unwrap();
    connection.with(|sqlite| {
        sqlite.execute_batch("DROP TABLE _foreign_server_metadata; UPDATE _metadata SET value='48' WHERE key='schema_version'; CREATE TRIGGER reject_server_upgrade BEFORE INSERT ON _metadata WHEN NEW.key='schema_version' AND NEW.value='50' BEGIN SELECT RAISE(ABORT, 'reject upgrade'); END;")?;
        Ok(())
    }).unwrap();
    assert!(Catalog::open(connection.clone()).is_err());
    connection
        .with(|sqlite| {
            assert_eq!(
                sqlite.query_row(
                    "SELECT value FROM _metadata WHERE key='schema_version'",
                    [],
                    |row| row.get::<_, String>(0)
                )?,
                "48"
            );
            assert!(!table_exists(sqlite, "_foreign_server_metadata")?);
            sqlite.execute_batch("DROP TRIGGER reject_server_upgrade")?;
            Ok(())
        })
        .unwrap();
    let upgraded = Catalog::open(connection.clone()).unwrap();
    assert_eq!(upgraded.load_foreign_server_rows().unwrap(), [server(None)]);
    assert_eq!(
        Catalog::open(connection)
            .unwrap()
            .load_foreign_server_rows()
            .unwrap(),
        [server(None)]
    );
}
