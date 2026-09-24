//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Singleton upgrade preserves identities and rolls back together with admission failures.

use rusqlite::params;

use super::*;

fn legacy(store: &SQLiteRecordStore, control: &StorageReadControl) -> SerializableTransactionId {
    let actor = admit_read(store, b"retained-before-record-upgrade");
    let held = store.serializable_admission(control).unwrap();
    let mut bytes = Vec::new();
    held.graph().write_checkpoint(&mut bytes, control).unwrap();
    held.connection
        .execute_batch("DROP TABLE _uqa_serializable_records; DROP TABLE _uqa_serializable_state")
        .unwrap();
    held.connection
        .execute_batch(super::super::schema::LEGACY_DEFINITION)
        .unwrap();
    held.connection
        .execute(
            "INSERT INTO _uqa_serializable_state VALUES (1, ?1, ?2, ?3)",
            params![actor.database().as_bytes(), actor.coordinator(), bytes],
        )
        .unwrap();
    held.connection
        .pragma_update(None, "user_version", 1)
        .unwrap();
    held.connection.execute_batch("COMMIT").unwrap();
    actor
}

#[test]
fn singleton_upgrade_is_atomic_in_every_sqlite_storage_mode() {
    for mode in 0..5 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("upgrade.db");
        let connection = if mode == 4 {
            ManagedConnection::open_in_memory().unwrap()
        } else {
            open(&path, mode)
        };
        let store = SQLiteRecordStore::new(&connection).unwrap();
        let control = control();
        let actor = legacy(&store, &control);
        for cancel in [false, true] {
            let held = store.serializable_admission(&control).unwrap();
            held.graph().check_active(actor).unwrap();
            assert!(held.graph().checkpoint_records_changed());
            if cancel {
                control.cancellation().cancel();
                assert!(held.persist(&control).is_err());
                control.cancellation().reset();
            } else {
                drop(held);
            }
            let auxiliary = store
                .connection
                .serializable_connection(store.identity, &control)
                .unwrap();
            assert_eq!(
                auxiliary
                    .lease_connection()
                    .unwrap()
                    .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                    .unwrap(),
                1
            );
        }
        store
            .serializable_admission(&control)
            .unwrap()
            .persist(&control)
            .unwrap();
        let mut held = store.serializable_admission(&control).unwrap();
        assert!(!held.graph().checkpoint_records_changed());
        held.graph().check_active(actor).unwrap();
        let next = held.graph_mut().admit(true, &control).unwrap();
        assert_eq!(next.coordinator(), actor.coordinator());
        assert!(next.allocation() > actor.allocation());
        held.persist(&control).unwrap();
    }
}

#[test]
fn malformed_singletons_are_not_recreated_as_empty_record_sets() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let control = control();
    legacy(&store, &control);
    let auxiliary = store
        .connection
        .serializable_connection(store.identity, &control)
        .unwrap();
    auxiliary
        .lease_connection()
        .unwrap()
        .execute("UPDATE _uqa_serializable_state SET checkpoint = x'00'", [])
        .unwrap();
    assert!(store.serializable_admission(&control).is_err());
    let connection = auxiliary.lease_connection().unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name = '_uqa_serializable_records'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}
