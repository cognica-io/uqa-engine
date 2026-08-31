//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! INSERT conflict reservation and committed-row refresh coverage.

use super::*;

#[test]
fn insert_returning_rebuilds_after_a_concurrent_conflict_commits() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(
        &directory
            .path()
            .join("insert-returning-concurrent-conflict.db"),
    )
    .unwrap();
    root.sql(
        "CREATE TABLE concurrent_conflict_source (id INTEGER PRIMARY KEY, value INTEGER); CREATE TABLE concurrent_conflict_target (id INTEGER PRIMARY KEY, value INTEGER); INSERT INTO concurrent_conflict_source VALUES (1, 10), (101, 1010)",
        &[],
    )
    .unwrap();
    let writer = root.new_session().unwrap();
    let inserter = root.new_session().unwrap();
    let gate = Arc::new(Barrier::new(2));
    let callback_gate = Arc::clone(&gate);
    let (entered_tx, entered_rx) = mpsc::channel();
    root.register_scalar_function_with_options(
        "insert_conflict_gate",
        SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
        move |_args: &[Value]| {
            entered_tx.send(()).unwrap();
            callback_gate.wait();
            Ok(Value::Int(1))
        },
    )
    .unwrap();
    writer.sql("BEGIN", &[]).unwrap();
    writer
        .sql(
            "INSERT INTO concurrent_conflict_target VALUES (1, 100)",
            &[],
        )
        .unwrap();

    let (done_tx, done_rx) = mpsc::channel();
    let insert_thread = std::thread::spawn(move || {
        done_tx
            .send(inserter.sql(
                "INSERT INTO concurrent_conflict_target VALUES (1, insert_conflict_gate()) ON CONFLICT (id) DO UPDATE SET value = concurrent_conflict_target.value + 1 RETURNING value, (SELECT value FROM concurrent_conflict_source AS source WHERE source.id = concurrent_conflict_target.value FOR UPDATE) AS locked_value",
                &[],
            ))
            .unwrap();
    });
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    writer.sql("COMMIT", &[]).unwrap();
    gate.wait();
    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    insert_thread.join().unwrap();
    assert_eq!(result.rows[0]["value"], Value::Int(101));
    assert_eq!(result.rows[0]["locked_value"], Value::Int(1010));
}
