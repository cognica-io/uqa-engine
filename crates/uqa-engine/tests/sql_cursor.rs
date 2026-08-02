//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::Value;
use uqa_engine::Engine;
use uqa_execution::DEFAULT_BATCH_SIZE;

#[test]
fn cursor_spills_under_tiny_work_mem_and_yields_bounded_column_batches() {
    let engine = Engine::new();
    engine.sql("SET work_mem TO '1B'", &[]).unwrap();

    let mut cursor = engine
        .sql_cursor(
            "SELECT value FROM generate_series(1, 5000) AS series(value) ORDER BY value",
            &[],
        )
        .unwrap();
    assert_eq!(cursor.columns(), ["value"]);
    assert_eq!(cursor.row_count(), 5_000);
    assert!(cursor.spilled_to_disk());

    let mut values = Vec::new();
    let mut largest_batch = 0;
    for batch in &mut cursor {
        let batch = batch.unwrap();
        largest_batch = largest_batch.max(batch.len());
        values.extend(batch.columns()[0].values.iter().cloned());
    }
    assert!(largest_batch <= DEFAULT_BATCH_SIZE, "{largest_batch}");
    assert_eq!(values.len(), 5_000);
    assert_eq!(values.first(), Some(&Value::Int(1)));
    assert_eq!(values.last(), Some(&Value::Int(5_000)));
}

#[test]
fn columnar_callback_consumes_each_batch_without_result_row_materialization() {
    let engine = Engine::new();
    let mut values = Vec::new();
    let summary = engine
        .sql_columnar("VALUES (1, 'a'), (2, 'b'), (3, 'c')", &[], |batch| {
            assert_eq!(batch.columns().len(), 2);
            values.extend(batch.columns()[0].values.iter().cloned());
            Ok(())
        })
        .unwrap();

    assert_eq!(summary.columns, ["column1", "column2"]);
    assert_eq!(summary.row_count, 3);
    assert_eq!(values, [Value::Int(1), Value::Int(2), Value::Int(3)]);
}

#[test]
fn cursor_rejects_commands_and_multi_statement_batches_before_execution() {
    let engine = Engine::new();
    let command = engine.sql_cursor("CREATE TABLE should_not_exist (id BIGINT)", &[]);
    assert!(command
        .err()
        .expect("command must be rejected")
        .to_string()
        .contains("exactly one query"));
    assert!(!engine.has_table("should_not_exist").unwrap());

    let batch = engine.sql_cursor("SELECT 1; SELECT 2", &[]);
    assert!(batch
        .err()
        .expect("batch must be rejected")
        .to_string()
        .contains("received 2"));
}

#[test]
fn persistent_cursor_releases_its_statement_snapshot_before_consumption() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("cursor-snapshot.db");
    let reader = Engine::open(&database).unwrap();
    reader
        .sql(
            "CREATE TABLE items (id BIGINT PRIMARY KEY); INSERT INTO items VALUES (1), (2)",
            &[],
        )
        .unwrap();

    let cursor = reader
        .sql_cursor("SELECT id FROM items ORDER BY id", &[])
        .unwrap();
    assert_eq!(reader.transaction_depth(), 0);

    let writer = Engine::open(&database).unwrap();
    writer.sql("INSERT INTO items VALUES (3)", &[]).unwrap();

    let values = cursor
        .map(|batch| batch.unwrap().columns()[0].values.clone())
        .collect::<Vec<_>>()
        .concat();
    assert_eq!(values, [Value::Int(1), Value::Int(2)]);
    assert_eq!(
        writer
            .sql("SELECT count(*) AS count FROM items", &[])
            .unwrap()
            .rows[0]["count"],
        Value::Int(3)
    );
}
