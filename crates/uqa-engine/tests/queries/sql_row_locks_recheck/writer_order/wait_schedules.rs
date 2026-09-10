//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn insert_conflict_wait_precedes_the_data_writer() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("insert-conflict-writer-order.db")).unwrap();
    root.sql(
        "CREATE TABLE conflict_writer_order (id INTEGER PRIMARY KEY, value INTEGER); INSERT INTO conflict_writer_order VALUES (1, 0)",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let inserter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql(
            "SELECT id FROM conflict_writer_order WHERE id = 1 FOR UPDATE",
            &[],
        )
        .unwrap();

    let (done_tx, done_rx) = mpsc::channel();
    let insert_thread = std::thread::spawn(move || {
        done_tx
            .send(inserter.sql(
                "INSERT INTO conflict_writer_order VALUES (1, 9) ON CONFLICT DO NOTHING",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql("DELETE FROM conflict_writer_order WHERE id = 1", &[])
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();
    let insert_result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    insert_thread.join().unwrap();
    assert_eq!(insert_result.affected_rows, 1);
}

#[test]
fn foreign_key_wait_precedes_the_data_writer() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("foreign-key-writer-order.db")).unwrap();
    root.sql(
        "CREATE TABLE parent_writer_order (id INTEGER PRIMARY KEY, value INTEGER); CREATE TABLE child_writer_order (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES parent_writer_order(id)); INSERT INTO parent_writer_order VALUES (1, 0)",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let inserter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql(
            "SELECT id FROM parent_writer_order WHERE id = 1 FOR UPDATE",
            &[],
        )
        .unwrap();

    let (done_tx, done_rx) = mpsc::channel();
    let insert_thread = std::thread::spawn(move || {
        done_tx
            .send(inserter.sql("INSERT INTO child_writer_order VALUES (10, 1)", &[]))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql("UPDATE parent_writer_order SET value = 1 WHERE id = 1", &[])
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();
    done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    insert_thread.join().unwrap();
}

#[test]
fn multirow_insert_foreign_keys_precede_the_data_writer() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("multirow-insert-fk-writer-order.db")).unwrap();
    root.sql(
        "CREATE TABLE multirow_fk_parent (id INTEGER PRIMARY KEY, value INTEGER); CREATE TABLE multirow_fk_child (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES multirow_fk_parent(id)); INSERT INTO multirow_fk_parent VALUES (1, 0), (2, 0)",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let inserter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql(
            "SELECT id FROM multirow_fk_parent WHERE id = 2 FOR UPDATE",
            &[],
        )
        .unwrap();

    let (done_tx, done_rx) = mpsc::channel();
    let insert_thread = std::thread::spawn(move || {
        done_tx
            .send(inserter.sql("INSERT INTO multirow_fk_child VALUES (10, 1), (20, 2)", &[]))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql("UPDATE multirow_fk_parent SET value = 1 WHERE id = 2", &[])
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();
    done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    insert_thread.join().unwrap();
    assert_eq!(
        root.sql("SELECT count(*) AS n FROM multirow_fk_child", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(2)
    );
}

#[test]
fn multirow_insert_conflicts_precede_the_data_writer() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(
        &directory
            .path()
            .join("multirow-insert-conflict-writer-order.db"),
    )
    .unwrap();
    root.sql(
        "CREATE TABLE multirow_conflict_target (id INTEGER PRIMARY KEY, value INTEGER); INSERT INTO multirow_conflict_target VALUES (2, 0)",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let inserter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql(
            "SELECT id FROM multirow_conflict_target WHERE id = 2 FOR UPDATE",
            &[],
        )
        .unwrap();

    let (done_tx, done_rx) = mpsc::channel();
    let insert_thread = std::thread::spawn(move || {
        done_tx
            .send(inserter.sql(
                "INSERT INTO multirow_conflict_target VALUES (1, 1), (2, 2) ON CONFLICT (id) DO UPDATE SET value = EXCLUDED.value",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql(
            "UPDATE multirow_conflict_target SET value = 9 WHERE id = 2",
            &[],
        )
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();
    done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    insert_thread.join().unwrap();
    let rows = root
        .sql(
            "SELECT id, value FROM multirow_conflict_target ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(rows.rows[0]["value"], Value::Int(1));
    assert_eq!(rows.rows[1]["value"], Value::Int(2));
}

#[test]
fn multirow_update_foreign_keys_precede_the_data_writer() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("multirow-update-fk-writer-order.db")).unwrap();
    root.sql(
        "CREATE TABLE update_fk_parent (id INTEGER PRIMARY KEY, value INTEGER); CREATE TABLE update_fk_child (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES update_fk_parent(id)); INSERT INTO update_fk_parent VALUES (1, 0), (2, 0), (3, 0); INSERT INTO update_fk_child VALUES (1, 3), (2, 3)",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let updater = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql(
            "SELECT id FROM update_fk_parent WHERE id = 2 FOR UPDATE",
            &[],
        )
        .unwrap();

    let (done_tx, done_rx) = mpsc::channel();
    let update_thread = std::thread::spawn(move || {
        done_tx
            .send(updater.sql(
                "UPDATE update_fk_child SET parent_id = id WHERE id IN (1, 2)",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql("UPDATE update_fk_parent SET value = 1 WHERE id = 2", &[])
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();
    done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    update_thread.join().unwrap();
    let rows = root
        .sql("SELECT parent_id FROM update_fk_child ORDER BY id", &[])
        .unwrap();
    assert_eq!(rows.rows[0]["parent_id"], Value::Int(1));
    assert_eq!(rows.rows[1]["parent_id"], Value::Int(2));
}

#[test]
fn delete_cascade_dependencies_precede_the_data_writer() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("delete-cascade-writer-order.db")).unwrap();
    root.sql(
        "CREATE TABLE delete_parent (id INTEGER PRIMARY KEY, value INTEGER); CREATE TABLE delete_child (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES delete_parent(id) ON DELETE CASCADE, value INTEGER); INSERT INTO delete_parent VALUES (1, 0), (2, 0); INSERT INTO delete_child VALUES (10, 1, 0), (20, 2, 0)",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let deleter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql("SELECT id FROM delete_child WHERE id = 20 FOR UPDATE", &[])
        .unwrap();

    let (done_tx, done_rx) = mpsc::channel();
    let delete_thread = std::thread::spawn(move || {
        done_tx
            .send(deleter.sql("DELETE FROM delete_parent WHERE id IN (1, 2)", &[]))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql("UPDATE delete_child SET value = 1 WHERE id = 20", &[])
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();
    done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    delete_thread.join().unwrap();
    assert_eq!(
        root.sql("SELECT count(*) AS n FROM delete_child", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(0)
    );
}

#[test]
fn insert_select_dependencies_precede_the_data_writer() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("insert-select-fk-writer-order.db")).unwrap();
    root.sql(
        "CREATE TABLE select_fk_parent (id INTEGER PRIMARY KEY, value INTEGER); CREATE TABLE select_fk_source (id INTEGER PRIMARY KEY, parent_id INTEGER); CREATE TABLE select_fk_child (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES select_fk_parent(id)); INSERT INTO select_fk_parent VALUES (1, 0), (2, 0); INSERT INTO select_fk_source VALUES (10, 1), (20, 2)",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let inserter = root.new_session().unwrap();
    inserter.sql("SET work_mem TO '1B'", &[]).unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql(
            "SELECT id FROM select_fk_parent WHERE id = 2 FOR UPDATE",
            &[],
        )
        .unwrap();

    let (done_tx, done_rx) = mpsc::channel();
    let insert_thread = std::thread::spawn(move || {
        done_tx
            .send(inserter.sql(
                "INSERT INTO select_fk_child SELECT id, parent_id FROM select_fk_source ORDER BY id",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql("UPDATE select_fk_parent SET value = 1 WHERE id = 2", &[])
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();
    done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    insert_thread.join().unwrap();
    assert_eq!(
        root.sql("SELECT count(*) AS n FROM select_fk_child", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(2)
    );
}

#[test]
fn conflict_update_dependencies_precede_the_data_writer() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("conflict-update-fk-writer-order.db")).unwrap();
    root.sql(
        "CREATE TABLE conflict_fk_parent (id INTEGER PRIMARY KEY, value INTEGER); CREATE TABLE conflict_fk_target (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES conflict_fk_parent(id)); INSERT INTO conflict_fk_parent VALUES (1, 0), (2, 0); INSERT INTO conflict_fk_target VALUES (1, 1)",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let inserter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql(
            "SELECT id FROM conflict_fk_parent WHERE id = 2 FOR UPDATE",
            &[],
        )
        .unwrap();

    let (done_tx, done_rx) = mpsc::channel();
    let insert_thread = std::thread::spawn(move || {
        done_tx
            .send(inserter.sql(
                "INSERT INTO conflict_fk_target VALUES (1, 1) ON CONFLICT (id) DO UPDATE SET parent_id = 2",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql("UPDATE conflict_fk_parent SET value = 1 WHERE id = 2", &[])
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();
    done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    insert_thread.join().unwrap();
    assert_eq!(
        root.sql("SELECT parent_id FROM conflict_fk_target WHERE id = 1", &[],)
            .unwrap()
            .rows[0]["parent_id"],
        Value::Int(2)
    );
}

#[test]
fn merge_insert_dependencies_precede_the_data_writer() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("merge-insert-fk-writer-order.db")).unwrap();
    root.sql(
        "CREATE TABLE merge_fk_parent (id INTEGER PRIMARY KEY, value INTEGER); CREATE TABLE merge_fk_source (id INTEGER PRIMARY KEY, parent_id INTEGER); CREATE TABLE merge_fk_child (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES merge_fk_parent(id)); INSERT INTO merge_fk_parent VALUES (1, 0), (2, 0); INSERT INTO merge_fk_source VALUES (10, 1), (20, 2)",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let merger = root.new_session().unwrap();
    merger.sql("SET work_mem TO '1B'", &[]).unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql(
            "SELECT id FROM merge_fk_parent WHERE id = 2 FOR UPDATE",
            &[],
        )
        .unwrap();

    let (done_tx, done_rx) = mpsc::channel();
    let merge_thread = std::thread::spawn(move || {
        done_tx
            .send(merger.sql(
                "MERGE INTO merge_fk_child AS c USING merge_fk_source AS s ON c.id = s.id WHEN NOT MATCHED THEN INSERT (id, parent_id) VALUES (s.id, s.parent_id)",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql("UPDATE merge_fk_parent SET value = 1 WHERE id = 2", &[])
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();
    done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    merge_thread.join().unwrap();
    assert_eq!(
        root.sql("SELECT count(*) AS n FROM merge_fk_child", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(2)
    );
}

#[test]
fn create_table_as_locking_select_rechecks_concurrent_updates() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("ctas-lock-recheck.db")).unwrap();
    seed_accounts(&root);
    let holder = root.new_session().unwrap();
    let creator = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql("SELECT id FROM accounts WHERE id = 1 FOR UPDATE", &[])
        .unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let create_thread = std::thread::spawn(move || {
        done_tx
            .send(creator.sql(
                "CREATE TABLE snap AS SELECT id, balance FROM accounts WHERE id = 1 FOR UPDATE",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql("UPDATE accounts SET balance = 555 WHERE id = 1", &[])
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();

    done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    create_thread.join().unwrap();
    let snapped = root
        .sql("SELECT balance FROM snap WHERE id = 1", &[])
        .unwrap();
    assert_eq!(snapped.rows[0].get("balance"), Some(&Value::Int(555)));
}
