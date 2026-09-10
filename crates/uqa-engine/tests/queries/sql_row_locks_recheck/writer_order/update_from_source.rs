//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn update_from_recheck_keeps_the_source_row_selected_by_the_command_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("update-from-source-partner.db")).unwrap();
    root.sql("CREATE TABLE update_from_target (id INTEGER PRIMARY KEY, match_key INTEGER, value INTEGER); CREATE TABLE update_from_source (match_key INTEGER PRIMARY KEY, value INTEGER); INSERT INTO update_from_target VALUES (1, 1, 0); INSERT INTO update_from_source VALUES (1, 10), (2, 20)", &[]).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed_calls = Arc::clone(&calls);
    let gate = Arc::new(Barrier::new(2));
    let callback_gate = Arc::clone(&gate);
    let (entered_tx, entered_rx) = mpsc::channel();
    root.register_scalar_function_with_options(
        "update_from_source_gate",
        SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
        move |_args: &[Value]| {
            if observed_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                entered_tx.send(()).unwrap();
                callback_gate.wait();
            }
            Ok(Value::Int(1))
        },
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let updater = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql(
            "UPDATE update_from_target SET match_key = 2 WHERE id = 1",
            &[],
        )
        .unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let update_thread = std::thread::spawn(move || {
        done_tx
            .send(updater.sql("UPDATE update_from_target AS target SET value = source.value FROM update_from_source AS source WHERE target.id = 1 AND target.match_key = source.match_key AND update_from_source_gate() = 1", &[]))
            .unwrap();
    });
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    gate.wait();
    holder.sql("COMMIT", &[]).unwrap();
    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    update_thread.join().unwrap();

    assert_eq!(result.affected_rows, 0);
    let row = root
        .sql("SELECT match_key, value FROM update_from_target", &[])
        .unwrap();
    assert_eq!(row.value_at(0, 0), Some(&Value::Int(2)));
    assert_eq!(row.value_at(0, 1), Some(&Value::Int(0)));
}
