//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Committed referencing rows must be checked even when acquiring the parent lock does not wait.

use super::*;

fn action_after_child_commit(
    action: &str,
    foreign_key_action: &str,
) -> (
    Engine,
    Result<uqa_sql::SQLResult, uqa_sql::SQLError>,
    tempfile::TempDir,
) {
    action_after_child_commit_at_isolation(action, foreign_key_action, None)
}

fn action_after_child_commit_at_isolation(
    action: &str,
    foreign_key_action: &str,
    isolation: Option<&str>,
) -> (
    Engine,
    Result<uqa_sql::SQLResult, uqa_sql::SQLError>,
    tempfile::TempDir,
) {
    action_after_child_change(
        action,
        foreign_key_action,
        isolation,
        "INSERT INTO children VALUES (10, 1)",
    )
}

fn action_after_child_change(
    action: &str,
    foreign_key_action: &str,
    isolation: Option<&str>,
    child_change: &str,
) -> (
    Engine,
    Result<uqa_sql::SQLResult, uqa_sql::SQLError>,
    tempfile::TempDir,
) {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("foreign-key-snapshot.db")).unwrap();
    root.sql("CREATE TABLE parents (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    root.sql(&format!("CREATE TABLE children (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES parents(id) {foreign_key_action})"), &[]).unwrap();
    root.sql("INSERT INTO parents VALUES (1)", &[]).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(Barrier::new(2));
    let callback_gate = Arc::clone(&gate);
    let (entered_tx, entered_rx) = mpsc::channel();
    root.register_scalar_function_with_options(
        "foreign_key_scan_gate",
        SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
        move |_args: &[Value]| {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                entered_tx.send(()).unwrap();
                callback_gate.wait();
            }
            Ok(Value::Int(1))
        },
    )
    .unwrap();
    let inserter = root.new_session().unwrap();
    let actor = root.new_session().unwrap();
    if let Some(isolation) = isolation {
        actor
            .sql(&format!("BEGIN ISOLATION LEVEL {isolation}"), &[])
            .unwrap();
        actor.sql("SELECT count(*) FROM children", &[]).unwrap();
    }
    inserter.sql("BEGIN", &[]).unwrap();
    inserter.sql(child_change, &[]).unwrap();
    let action = format!("{action} WHERE id = 1 AND foreign_key_scan_gate() = 1");
    let explicit = isolation.is_some();
    let deferred = foreign_key_action.contains("INITIALLY DEFERRED");
    let action_thread = std::thread::spawn(move || {
        let result = actor.sql(&action, &[]);
        if deferred {
            assert!(
                result.is_ok(),
                "deferred action failed before COMMIT: {result:?}"
            );
        }
        if explicit && result.is_ok() {
            actor.sql("COMMIT", &[])?;
        }
        result
    });
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    inserter.sql("COMMIT", &[]).unwrap();
    gate.wait();
    let result = action_thread.join().unwrap();
    (root, result, directory)
}

#[test]
fn foreign_key_delete_checks_commits_before_lock_acquisition() {
    let (root, result, _directory) = action_after_child_commit("DELETE FROM parents", "");
    assert_eq!(result.unwrap_err().sqlstate(), Some("23503"));
    assert_eq!(
        root.sql("SELECT count(*) AS n FROM parents", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(1)
    );
}

#[test]
fn foreign_key_update_checks_commits_before_lock_acquisition() {
    let (root, result, _directory) = action_after_child_commit("UPDATE parents SET id = 2", "");
    assert_eq!(result.unwrap_err().sqlstate(), Some("23503"));
    assert_eq!(
        root.sql("SELECT id FROM parents", &[]).unwrap().rows[0]["id"],
        Value::Int(1)
    );
}

#[test]
fn foreign_key_delete_cascade_finds_commits_before_lock_acquisition() {
    let (root, result, _directory) =
        action_after_child_commit("DELETE FROM parents", "ON DELETE CASCADE");
    result.unwrap();
    assert_eq!(
        root.sql("SELECT count(*) AS n FROM children", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(0)
    );
}

#[test]
fn foreign_key_update_cascade_finds_commits_before_lock_acquisition() {
    let (root, result, _directory) =
        action_after_child_commit("UPDATE parents SET id = 2", "ON UPDATE CASCADE");
    result.unwrap();
    assert_eq!(
        root.sql("SELECT parent_id FROM children", &[])
            .unwrap()
            .rows[0]["parent_id"],
        Value::Int(2)
    );
}

#[test]
fn fixed_snapshot_foreign_key_delete_detects_new_commits() {
    for isolation in ["REPEATABLE READ", "SERIALIZABLE"] {
        for (action, expected) in [
            ("", "23503"),
            ("ON DELETE RESTRICT", "23503"),
            ("ON DELETE CASCADE", "40001"),
            ("ON DELETE SET NULL", "40001"),
            ("ON DELETE SET DEFAULT", "40001"),
            ("DEFERRABLE INITIALLY DEFERRED", "23503"),
        ] {
            let (root, result, _directory) = action_after_child_commit_at_isolation(
                "DELETE FROM parents",
                action,
                Some(isolation),
            );
            assert_eq!(
                result.unwrap_err().sqlstate(),
                Some(expected),
                "{isolation}: {action}"
            );
            assert_eq!(
                root.sql("SELECT count(*) AS n FROM parents", &[])
                    .unwrap()
                    .rows[0]["n"],
                Value::Int(1)
            );
        }
    }
}

#[test]
fn fixed_snapshot_foreign_key_update_detects_new_commits() {
    for isolation in ["REPEATABLE READ", "SERIALIZABLE"] {
        for (action, expected) in [
            ("", "23503"),
            ("ON UPDATE RESTRICT", "23503"),
            ("ON UPDATE CASCADE", "40001"),
            ("ON UPDATE SET NULL", "40001"),
            ("ON UPDATE SET DEFAULT", "40001"),
            ("DEFERRABLE INITIALLY DEFERRED", "23503"),
        ] {
            let (root, result, _directory) = action_after_child_commit_at_isolation(
                "UPDATE parents SET id = 2",
                action,
                Some(isolation),
            );
            assert_eq!(
                result.unwrap_err().sqlstate(),
                Some(expected),
                "{isolation}: {action}"
            );
            assert_eq!(
                root.sql("SELECT id FROM parents", &[]).unwrap().rows[0]["id"],
                Value::Int(1)
            );
        }
    }
}

#[test]
fn fixed_snapshot_reference_checks_ignore_nonmatching_commits() {
    for isolation in ["REPEATABLE READ", "SERIALIZABLE"] {
        let (root, result, _directory) = action_after_child_change(
            "DELETE FROM parents",
            "ON DELETE CASCADE",
            Some(isolation),
            "INSERT INTO children VALUES (10, NULL)",
        );
        result.unwrap();
        assert_eq!(
            root.sql("SELECT count(*) AS n FROM parents", &[])
                .unwrap()
                .rows[0]["n"],
            Value::Int(0)
        );
        assert_eq!(
            root.sql("SELECT count(*) AS n FROM children", &[])
                .unwrap()
                .rows[0]["n"],
            Value::Int(1)
        );
    }
}

#[test]
fn fixed_snapshot_cascades_include_own_inserted_children() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("foreign-key-own-writes.db")).unwrap();
    root.sql("CREATE TABLE parents (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    root.sql("CREATE TABLE children (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES parents(id) ON DELETE CASCADE)", &[]).unwrap();
    root.sql("INSERT INTO parents VALUES (1)", &[]).unwrap();
    for isolation in ["REPEATABLE READ", "SERIALIZABLE"] {
        let actor = root.new_session().unwrap();
        actor
            .sql(&format!("BEGIN ISOLATION LEVEL {isolation}"), &[])
            .unwrap();
        actor.sql("SELECT count(*) FROM children", &[]).unwrap();
        actor
            .sql("INSERT INTO children VALUES (10, 1)", &[])
            .unwrap();
        actor.sql("DELETE FROM parents", &[]).unwrap();
        assert_eq!(
            actor
                .sql("SELECT count(*) AS n FROM children", &[])
                .unwrap()
                .rows[0]["n"],
            Value::Int(0)
        );
        actor.sql("ROLLBACK", &[]).unwrap();
    }
}
