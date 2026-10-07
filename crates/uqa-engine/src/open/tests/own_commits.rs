//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::atomic::Ordering;

use uqa_core::Value;
use uqa_storage::ValueIndexKey;

use super::Engine;

#[rstest::rstest]
#[case::sqlite(0)]
#[case::sqlite_kv(1)]
#[case::redb(2)]
fn own_commit_adoption_opens_no_read_transaction_and_keeps_later_writers_visible(
    #[case] provider: usize,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("receipt-adoption.db");
    let engine = match provider {
        0 => Engine::open(&path).unwrap(),
        1 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(&path).unwrap(),
        ))
        .unwrap(),
        2 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_redb::RedbStorage::open(&path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    };
    pause_automatic_statistics(&engine);
    let peer = engine.new_session().unwrap();
    pause_automatic_statistics(&peer);
    engine
        .sql("CREATE TABLE receipts(id integer PRIMARY KEY)", &[])
        .unwrap();
    engine.sql("SELECT count(*) FROM receipts", &[]).unwrap();
    let before = engine.epochs.seen_storage_read_view.lock().clone().unwrap();
    engine.sql("INSERT INTO receipts VALUES (1)", &[]).unwrap();
    let backend = engine.storage.backend.as_ref().unwrap();
    // A new read transaction clears the completed receipt. Keeping it proves adoption did not open one just to reread the committed revision and generations.
    let committed = backend
        .committed_data_revision()
        .unwrap()
        .expect("adoption must retain the own commit receipt");
    assert!(committed.revision.follows_by_one_commit(&before));
    assert!(engine.epochs.seen_storage_read_view.lock().as_ref() == Some(&committed.revision));

    peer.sql("INSERT INTO receipts VALUES (2)", &[]).unwrap();
    // Put adoption immediately after this racing peer commit. The own receipt must never advance the observed view to the peer's newer sequence.
    *engine.epochs.seen_storage_read_view.lock() = Some(before);
    engine.adopt_own_commit_revisions();
    assert!(engine.epochs.seen_storage_read_view.lock().as_ref() == Some(&committed.revision));
    assert_eq!(
        scalar(&engine, "SELECT count(*) FROM receipts"),
        Value::Int(2)
    );
    peer.sql("ALTER TABLE receipts RENAME COLUMN id TO renamed", &[])
        .unwrap();
    assert_eq!(
        engine
            .sql("SELECT id FROM receipts", &[])
            .unwrap_err()
            .sqlstate(),
        Some("42703")
    );
    assert_eq!(
        scalar(&engine, "SELECT count(*) FROM receipts WHERE renamed > 0"),
        Value::Int(2)
    );
}

fn pause_automatic_statistics(engine: &Engine) {
    engine.release_automatic_statistics_client();
    engine
        .session
        .statistics_worker
        .store(true, Ordering::Release);
}

fn has_value_index(engine: &Engine, table: &str, key: &ValueIndexKey) -> bool {
    engine
        .try_table(table)
        .unwrap()
        .unwrap()
        .value_indexes
        .read()
        .contains_key(key)
}

fn scalar(engine: &Engine, sql: &str) -> Value {
    let result = engine.sql(sql, &[]).unwrap();
    result.value_at(0, 0).cloned().unwrap()
}

#[test]
fn own_data_commits_keep_value_indexes_until_another_writer_commits() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("own-commits.db");
    let engine = Engine::open(&path).unwrap();
    pause_automatic_statistics(&engine);
    engine
        .sql("CREATE TABLE kept (id INTEGER PRIMARY KEY, v INTEGER)", &[])
        .unwrap();
    engine
        .sql(
            "INSERT INTO kept SELECT g, g FROM generate_series(1, 50) AS g",
            &[],
        )
        .unwrap();
    let key = ValueIndexKey::from("id");
    let range = "SELECT count(*) FROM kept WHERE id BETWEEN 10 AND 19";
    assert_eq!(scalar(&engine, range), Value::Int(10));
    assert!(has_value_index(&engine, "kept", &key));

    // The session maintained the index for its own commit, which no other commit preceded.
    engine
        .sql("UPDATE kept SET v = 0 WHERE id = 15", &[])
        .unwrap();
    engine.sql("DELETE FROM kept WHERE id = 16", &[]).unwrap();
    engine.synchronize_table_data().unwrap();
    assert!(has_value_index(&engine, "kept", &key));
    assert_eq!(scalar(&engine, range), Value::Int(9));

    // Another writer's commit invalidates the session's view, so the index is rebuilt from committed rows.
    let other = Engine::open(&path).unwrap();
    pause_automatic_statistics(&other);
    other
        .sql("INSERT INTO kept VALUES (16, 1600)", &[])
        .unwrap();
    drop(other);
    engine.synchronize_table_data().unwrap();
    assert!(!has_value_index(&engine, "kept", &key));
    assert_eq!(scalar(&engine, range), Value::Int(10));
    assert_eq!(
        scalar(&engine, "SELECT v FROM kept WHERE id = 16"),
        Value::Int(1600)
    );
}

#[test]
fn own_commit_after_an_unobserved_external_commit_takes_the_ordinary_refresh() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("interleaved-commits.db");
    let engine = Engine::open(&path).unwrap();
    pause_automatic_statistics(&engine);
    engine
        .sql(
            "CREATE TABLE mixed (id INTEGER PRIMARY KEY, v INTEGER)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO mixed SELECT g, g FROM generate_series(1, 20) AS g",
            &[],
        )
        .unwrap();
    let key = ValueIndexKey::from("id");
    let range = "SELECT count(*) FROM mixed WHERE id BETWEEN 1 AND 30";
    assert_eq!(scalar(&engine, range), Value::Int(20));
    assert!(has_value_index(&engine, "mixed", &key));

    // The external row commits before the session's own transaction, which never observed it.
    engine.sql("BEGIN", &[]).unwrap();
    let other = Engine::open(&path).unwrap();
    pause_automatic_statistics(&other);
    other
        .sql("INSERT INTO mixed VALUES (25, 250)", &[])
        .unwrap();
    drop(other);
    engine
        .sql("INSERT INTO mixed VALUES (26, 260)", &[])
        .unwrap();
    engine.sql("COMMIT", &[]).unwrap();
    engine.synchronize_table_data().unwrap();
    assert!(!has_value_index(&engine, "mixed", &key));
    assert_eq!(scalar(&engine, range), Value::Int(22));
}

#[test]
fn rolled_back_own_writes_do_not_survive_in_value_indexes() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("rolled-back.db")).unwrap();
    pause_automatic_statistics(&engine);
    engine
        .sql(
            "CREATE TABLE undone (id INTEGER PRIMARY KEY, v INTEGER)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO undone SELECT g, g FROM generate_series(1, 20) AS g",
            &[],
        )
        .unwrap();
    let range = "SELECT count(*) FROM undone WHERE id BETWEEN 1 AND 100";
    assert_eq!(scalar(&engine, range), Value::Int(20));
    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql(
            "INSERT INTO undone SELECT g, g FROM generate_series(50, 59) AS g",
            &[],
        )
        .unwrap();
    engine.sql("DELETE FROM undone WHERE id = 3", &[]).unwrap();
    assert_eq!(scalar(&engine, range), Value::Int(29));
    engine.sql("ROLLBACK", &[]).unwrap();
    assert_eq!(scalar(&engine, range), Value::Int(20));
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) FROM undone WHERE id BETWEEN 50 AND 59"
        ),
        Value::Int(0)
    );
    assert_eq!(
        scalar(&engine, "SELECT v FROM undone WHERE id = 3"),
        Value::Int(3)
    );
}
