//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::atomic::Ordering;
use std::sync::Arc;

use super::Engine;

fn pause_automatic_statistics(engine: &Engine) {
    engine.release_automatic_statistics_client();
    engine
        .session
        .statistics_worker
        .store(true, Ordering::Release);
}

#[test]
fn data_commits_retain_schema_and_decoded_statistics_across_sessions() {
    let directory = tempfile::tempdir().unwrap();
    let writer = Engine::open(&directory.path().join("data-refresh.db")).unwrap();
    pause_automatic_statistics(&writer);
    writer.sql("CREATE TABLE changed (id INTEGER PRIMARY KEY, body TEXT); CREATE TABLE untouched (id INTEGER PRIMARY KEY); INSERT INTO changed VALUES (1, 'first'); ANALYZE changed", &[]).unwrap();
    let reader = writer.new_session().unwrap();
    pause_automatic_statistics(&reader);
    reader.sql("SELECT * FROM changed", &[]).unwrap();
    let before = reader.require_table("changed").unwrap();
    let schema = before.columns.snapshot();
    let statistics = before.column_stats.snapshot();
    let unrelated = reader.require_table("untouched").unwrap();
    let registries = reader.durable.schemas.snapshot();
    for id in 2..12 {
        writer
            .sql(&format!("INSERT INTO changed VALUES ({id}, 'next')"), &[])
            .unwrap();
        assert_eq!(
            reader.sql("SELECT * FROM changed", &[]).unwrap().rows.len(),
            id as usize
        );
        let after = reader.require_table("changed").unwrap();
        assert!(
            Arc::ptr_eq(&before, &after),
            "data-only commit rebuilt the table catalog"
        );
        assert!(Arc::ptr_eq(&schema, &after.columns.snapshot()));
        assert!(
            Arc::ptr_eq(&statistics, &after.column_stats.snapshot()),
            "data-only commit decoded unchanged statistics"
        );
        assert!(Arc::ptr_eq(
            &unrelated,
            &reader.require_table("untouched").unwrap()
        ));
        assert!(Arc::ptr_eq(&registries, &reader.durable.schemas.snapshot()));
    }
}

#[test]
fn analyze_replaces_only_affected_statistics_and_shares_the_decoded_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let writer = Engine::open(&directory.path().join("stats-refresh.db")).unwrap();
    pause_automatic_statistics(&writer);
    writer.sql("CREATE TABLE changed (id INTEGER PRIMARY KEY); CREATE TABLE untouched (id INTEGER PRIMARY KEY); INSERT INTO changed VALUES (1); ANALYZE", &[]).unwrap();
    let first = writer.new_session().unwrap();
    let second = writer.new_session().unwrap();
    pause_automatic_statistics(&first);
    pause_automatic_statistics(&second);
    let first_table = first.require_table("changed").unwrap();
    let second_table = second.require_table("changed").unwrap();
    let unrelated_stats = first
        .require_table("untouched")
        .unwrap()
        .column_stats
        .snapshot();
    writer
        .sql("INSERT INTO changed VALUES (2); ANALYZE changed", &[])
        .unwrap();
    first.sql("SELECT * FROM changed", &[]).unwrap();
    second.sql("SELECT * FROM changed", &[]).unwrap();
    assert!(Arc::ptr_eq(
        &first_table,
        &first.require_table("changed").unwrap()
    ));
    assert!(Arc::ptr_eq(
        &second_table,
        &second.require_table("changed").unwrap()
    ));
    assert_eq!(first_table.column_stats.read()["id"].row_count, 2);
    assert!(Arc::ptr_eq(
        &first_table.column_stats.snapshot(),
        &second_table.column_stats.snapshot()
    ));
    assert!(Arc::ptr_eq(
        &unrelated_stats,
        &first
            .require_table("untouched")
            .unwrap()
            .column_stats
            .snapshot()
    ));
}

#[test]
fn independent_engine_refresh_observes_data_and_ddl_without_rebuilding_stable_tables() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("independent-refresh.db");
    let writer = Engine::open(&path).unwrap();
    pause_automatic_statistics(&writer);
    writer
        .sql("CREATE TABLE items (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    let reader = Engine::open(&path).unwrap();
    pause_automatic_statistics(&reader);
    reader.sql("SELECT * FROM items", &[]).unwrap();
    let before = reader.require_table("items").unwrap();
    writer.sql("INSERT INTO items VALUES (1)", &[]).unwrap();
    assert_eq!(
        reader.sql("SELECT * FROM items", &[]).unwrap().rows.len(),
        1
    );
    assert!(Arc::ptr_eq(
        &before,
        &reader.require_table("items").unwrap()
    ));
    writer
        .sql("ALTER TABLE items ADD COLUMN extra TEXT", &[])
        .unwrap();
    assert!(reader.sql("SELECT extra FROM items", &[]).is_ok());
    writer
        .sql(
            "BEGIN; ALTER TABLE items ADD COLUMN private TEXT; ROLLBACK",
            &[],
        )
        .unwrap();
    assert!(reader.sql("SELECT private FROM items", &[]).is_err());
}

#[test]
fn automatic_statistics_and_concurrent_readers_retain_table_definitions() {
    use std::time::{Duration, Instant};

    let directory = tempfile::tempdir().unwrap();
    let writer = Engine::open(&directory.path().join("automatic-refresh.db")).unwrap();
    writer
        .sql(
            "CREATE TABLE items (id INTEGER PRIMARY KEY, body TEXT)",
            &[],
        )
        .unwrap();
    writer.begin().unwrap();
    for id in 0..80 {
        writer
            .sql(
                "INSERT INTO items VALUES ($1, 'sample body')",
                &[uqa_sql::SQLParam::Scalar(uqa_core::Value::Int(id))],
            )
            .unwrap();
    }
    writer.commit().unwrap();
    let readers = (0..4)
        .map(|_| writer.new_session().unwrap())
        .collect::<Vec<_>>();
    let completed_before = writer.automatic_statistics_status().completed;
    std::thread::scope(|scope| {
        let tasks = readers
            .iter()
            .map(|reader| {
                scope.spawn(move || {
                    let table = reader.require_table("items").unwrap();
                    let columns = table.columns.snapshot();
                    let deadline = Instant::now() + Duration::from_secs(20);
                    loop {
                        let result = reader.sql("SELECT count(*) AS n FROM items", &[]).unwrap();
                        assert_eq!(result.rows[0]["n"], uqa_core::Value::Int(80));
                        let current = reader.require_table("items").unwrap();
                        assert!(Arc::ptr_eq(&table, &current));
                        assert!(Arc::ptr_eq(&columns, &current.columns.snapshot()));
                        if current
                            .column_stats
                            .read()
                            .get("id")
                            .is_some_and(|stats| stats.row_count == 80)
                        {
                            break;
                        }
                        assert!(
                            Instant::now() < deadline,
                            "automatic statistics did not refresh: {:?}",
                            reader.automatic_statistics_status()
                        );
                        std::thread::sleep(Duration::from_millis(10));
                    }
                })
            })
            .collect::<Vec<_>>();
        for task in tasks {
            task.join().unwrap();
        }
    });
    assert!(writer.automatic_statistics_status().last_error.is_none());
    assert!(writer.automatic_statistics_status().completed >= completed_before);
}
