//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cursor relation-lock lifetime coverage.

use super::*;

#[test]
fn pg18_cursor_declaration_holds_access_share_until_transaction_end() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("cursor-relation-lock.db")).unwrap();
    root.sql("CREATE TABLE cursor_locked_relation (id INTEGER)", &[])
        .unwrap();
    let ddl = root.new_session().unwrap();

    root.sql("BEGIN", &[]).unwrap();
    root.sql(
        "DECLARE relation_lock_cursor CURSOR FOR SELECT id FROM cursor_locked_relation",
        &[],
    )
    .unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let ddl_thread = std::thread::spawn(move || {
        sender
            .send(ddl.sql("DROP TABLE cursor_locked_relation", &[]))
            .unwrap();
    });
    assert!(
        receiver
            .recv_timeout(std::time::Duration::from_millis(100))
            .is_err(),
        "DROP TABLE passed a cursor's AccessShare relation lock"
    );
    root.sql("ROLLBACK", &[]).unwrap();
    receiver
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("DROP TABLE remained blocked after cursor transaction end")
        .unwrap();
    ddl_thread.join().unwrap();

    root.sql(
        "CREATE TABLE cursor_locked_operator_left (id INTEGER PRIMARY KEY, embedding VECTOR(2)); \
         CREATE TABLE cursor_locked_operator_right (id INTEGER PRIMARY KEY, archived_embedding VECTOR(2))",
        &[],
    )
    .unwrap();
    let operator_left_ddl = root.new_session().unwrap();
    let operator_right_ddl = root.new_session().unwrap();
    root.sql("BEGIN", &[]).unwrap();
    root.sql(
        "DECLARE operator_relation_lock_cursor CURSOR FOR SELECT left_doc_id FROM vector_similarity_join(cursor_locked_operator_left, knn_match(embedding, ARRAY[1.0, 0.0], 1), cursor_locked_operator_right, knn_match(archived_embedding, ARRAY[1.0, 0.0], 1), 0.8)",
        &[],
    )
    .unwrap();
    let (operator_left_sender, operator_left_receiver) = std::sync::mpsc::channel();
    let operator_left_ddl_thread = std::thread::spawn(move || {
        operator_left_sender
            .send(operator_left_ddl.sql("DROP TABLE cursor_locked_operator_left", &[]))
            .unwrap();
    });
    let (operator_right_sender, operator_right_receiver) = std::sync::mpsc::channel();
    let operator_right_ddl_thread = std::thread::spawn(move || {
        operator_right_sender
            .send(operator_right_ddl.sql("DROP TABLE cursor_locked_operator_right", &[]))
            .unwrap();
    });
    assert!(
        operator_left_receiver
            .recv_timeout(std::time::Duration::from_millis(100))
            .is_err(),
        "DROP TABLE passed an operator cursor's left AccessShare relation lock"
    );
    assert!(
        operator_right_receiver
            .recv_timeout(std::time::Duration::from_millis(100))
            .is_err(),
        "DROP TABLE passed an operator cursor's right AccessShare relation lock"
    );
    root.sql("ROLLBACK", &[]).unwrap();
    operator_left_receiver
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("operator left relation DROP TABLE remained blocked after cursor transaction end")
        .unwrap();
    operator_right_receiver
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("operator right relation DROP TABLE remained blocked after cursor transaction end")
        .unwrap();
    operator_left_ddl_thread.join().unwrap();
    operator_right_ddl_thread.join().unwrap();
}
