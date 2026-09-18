//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! DROP target binding, authorization and direct-API waits across persistent providers.

use super::{
    relation_lock_support::{
        after_operation_wait, after_wait, error, sessions, sql, wait_for_relation,
    },
    *,
};
use std::{sync::mpsc, thread, time::Duration};

#[test]
fn drop_rechecks_owner_after_waiting_on_the_same_relation() {
    for provider in 0..3 {
        for (kind, create) in [
            ("TABLE", "CREATE TABLE v(v integer)"),
            ("VIEW", "CREATE VIEW v AS SELECT 1 AS v"),
            (
                "MATERIALIZED VIEW",
                "CREATE MATERIALIZED VIEW v AS SELECT 1 AS v",
            ),
            (
                "FOREIGN TABLE",
                "CREATE SERVER source FOREIGN DATA WRAPPER memory_fdw; CREATE FOREIGN TABLE v(v integer) SERVER source",
            ),
        ] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE ROLE before_owner; CREATE ROLE after_owner; GRANT CREATE ON SCHEMA public TO before_owner, after_owner");
            sql(&first, create);
            sql(&first, &format!("ALTER {kind} v OWNER TO before_owner"));
            sql(&second, "SET ROLE before_owner");
            sql(
                &first,
                &format!("BEGIN; ALTER {kind} v OWNER TO after_owner"),
            );
            let (_, result) = after_wait(
                &first,
                second,
                &format!("DROP {kind} v"),
                "public.v",
                "COMMIT",
            );
            assert_eq!(result.unwrap_err().sqlstate(), Some("42501"));
            assert_ne!(
                sql(&first, "SELECT to_regclass('v') AS relation").rows[0]["relation"],
                Value::Null
            );
        }
    }
}

#[test]
fn drop_rebinds_a_renamed_missing_or_reused_view_name() {
    for provider in 0..3 {
        for (change, drop, expected) in [
            ("ALTER VIEW v RENAME TO moved", "DROP VIEW v", Some("42P01")),
            (
                "ALTER VIEW v RENAME TO moved",
                "DROP VIEW IF EXISTS v",
                None,
            ),
            ("DROP VIEW v", "DROP VIEW IF EXISTS v", None),
            (
                "DROP VIEW v; CREATE TABLE v(v integer)",
                "DROP VIEW IF EXISTS v",
                Some("42809"),
            ),
            (
                "DROP VIEW v; CREATE VIEW v AS SELECT 2 AS v",
                "DROP VIEW v",
                None,
            ),
        ] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE VIEW v AS SELECT 1 AS v");
            sql(&first, "BEGIN");
            sql(&first, change);
            let (second, result) = after_wait(&first, second, drop, "public.v", "COMMIT");
            if let Some(state) = expected {
                assert_eq!(result.unwrap_err().sqlstate(), Some(state));
            } else {
                result.unwrap();
            }
            if drop.contains("IF EXISTS") && expected.is_none() {
                let notices = second.query_runtime_view().notices.lock().clone();
                assert_eq!(
                    notices
                        .iter()
                        .filter(|(_, text)| text == "view \"v\" does not exist, skipping")
                        .count(),
                    1
                );
            }
            if change.starts_with("ALTER") {
                assert_eq!(
                    sql(&second, "SELECT * FROM moved").rows[0]["v"],
                    Value::Int(1)
                );
            } else if change.contains("CREATE TABLE") {
                sql(&second, "INSERT INTO v VALUES (3)");
                assert_eq!(sql(&second, "SELECT * FROM v").rows[0]["v"], Value::Int(3));
            } else {
                assert_eq!(
                    sql(&second, "SELECT to_regclass('v') AS relation").rows[0]["relation"],
                    Value::Null
                );
            }
        }
    }
}

#[test]
fn direct_view_drop_waits_and_revalidates_owner_and_name() {
    for provider in 0..3 {
        for scenario in 0..3 {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE VIEW v AS SELECT 1 AS v");
            match scenario {
                0 => {
                    sql(&first, "BEGIN; SELECT * FROM v");
                }
                1 => {
                    sql(&first, "BEGIN; ALTER VIEW v RENAME TO moved");
                }
                _ => {
                    sql(&first, "CREATE ROLE before_owner; CREATE ROLE after_owner; GRANT CREATE ON SCHEMA public TO before_owner, after_owner; ALTER VIEW v OWNER TO before_owner");
                    sql(&second, "SET ROLE before_owner");
                    sql(&first, "BEGIN; ALTER VIEW v OWNER TO after_owner");
                }
            }
            let (_, result) =
                after_operation_wait(&first, second, "public.v", "COMMIT", |engine| {
                    engine.drop_view("v")
                });
            match scenario {
                0 => {
                    assert!(result.unwrap());
                }
                1 => {
                    assert!(!result.unwrap());
                    assert_eq!(
                        sql(&first, "SELECT * FROM moved").rows[0]["v"],
                        Value::Int(1)
                    );
                }
                _ => {
                    assert_eq!(result.unwrap_err().sqlstate(), Some("42501"));
                    assert_eq!(
                        first
                            .view_definition("v")
                            .unwrap()
                            .unwrap()
                            .security
                            .resolve(&first.durable.roles.read())
                            .unwrap()
                            .role_owner,
                        "after_owner"
                    );
                }
            }
        }
    }
}

#[test]
fn drop_rejects_wrong_kind_and_missing_authority_before_waiting() {
    for provider in 0..3 {
        for (statement, state) in [("DROP TABLE v", "42809"), ("DROP VIEW v", "42501")] {
            let (_directory, first, second) = sessions(provider);
            sql(
                &first,
                "CREATE ROLE outsider; CREATE VIEW v AS SELECT 1 AS v",
            );
            sql(&second, "SET ROLE outsider");
            sql(&first, "BEGIN; SELECT * FROM v");
            let session = second.session_id;
            let cancellation = second.runtime.cancellation.clone();
            let task = thread::spawn(move || second.sql(statement, &[]));
            let waited = wait_for_relation(&first, session, "public.v", || task.is_finished());
            cancellation.cancel();
            sql(&first, "COMMIT");
            let result = task.join().unwrap();
            assert!(!waited, "{statement} must reject without waiting");
            assert_eq!(result.unwrap_err().sqlstate(), Some(state));
        }
    }
}

#[test]
fn drop_locks_multiple_targets_in_statement_order() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(
            &first,
            "CREATE TABLE a(v integer); CREATE TABLE z(v integer)",
        );
        let observer = first.new_session().unwrap();
        sql(&first, "BEGIN; LOCK TABLE a IN ACCESS EXCLUSIVE MODE");
        let session = second.session_id;
        let cancel = second.runtime.cancellation.clone();
        let (send, done) = mpsc::channel();
        let task = thread::spawn(move || {
            let _ = send.send(second.sql("DROP TABLE z, a", &[]));
        });
        let waited = wait_for_relation(&first, session, "public.a", || task.is_finished());
        sql(&observer, "BEGIN");
        let blocked = observer.sql("LOCK TABLE z IN ACCESS SHARE MODE NOWAIT", &[]);
        sql(&observer, "ROLLBACK");
        sql(&first, "COMMIT");
        let result = done.recv_timeout(Duration::from_secs(30));
        if result.is_err() {
            cancel.cancel();
        }
        task.join().unwrap();
        assert!(waited);
        assert_eq!(blocked.unwrap_err().sqlstate(), Some("55P03"));
        result.unwrap().unwrap();
        assert_eq!(
            sql(&first, "SELECT to_regclass('z') AS relation").rows[0]["relation"],
            Value::Null
        );
    }
}

#[test]
fn drop_handles_duplicate_targets_and_reports_missing_schema_notices() {
    let engine = Engine::new();
    sql(&engine, "CREATE VIEW v AS SELECT 1 AS v; CREATE VIEW dependent AS SELECT * FROM v; DROP VIEW v, dependent, public.v");
    sql(&engine, "CREATE TABLE t(v integer); DROP TABLE t, public.t");
    for kind in ["VIEW", "MATERIALIZED VIEW", "SEQUENCE", "FOREIGN TABLE"] {
        error(&engine, &format!("DROP {kind} absent.item"), "3F000");
        sql(&engine, &format!("DROP {kind} IF EXISTS absent.item"));
        assert!(engine
            .query_runtime_view()
            .notices
            .lock()
            .iter()
            .any(|(_, message)| message == "schema \"absent\" does not exist, skipping"));
    }
    error(&engine, "DROP FOREIGN TABLE missing", "42704");
}

const DEPENDENCY_ROOTS: [(&str, &str); 4] = [
    ("TABLE", "CREATE TABLE a(v integer); INSERT INTO a VALUES (1)"),
    ("VIEW", "CREATE VIEW a AS SELECT 1 AS v"),
    ("MATERIALIZED VIEW", "CREATE MATERIALIZED VIEW a AS SELECT 1 AS v"),
    ("FOREIGN TABLE", "CREATE SERVER source FOREIGN DATA WRAPPER memory_fdw; CREATE FOREIGN TABLE a(v integer) SERVER source"),
];

fn create_dependency_chain(engine: &Engine, create_root: &str) {
    sql(engine, create_root);
    sql(engine, "CREATE VIEW b AS SELECT 2 AS v; CREATE VIEW d AS SELECT * FROM a; CREATE VIEW outer_view AS SELECT * FROM d");
}

#[test]
fn relation_drop_waits_for_view_dependencies_and_preserves_removed_edges() {
    for provider in 0..3 {
        for (kind, create) in DEPENDENCY_ROOTS {
            for behavior in ["CASCADE", "RESTRICT"] {
                let (_directory, first, second) = sessions(provider);
                create_dependency_chain(&first, create);
                sql(&first, "BEGIN; CREATE OR REPLACE VIEW d AS SELECT * FROM b");
                let (second, result) = after_wait(
                    &first,
                    second,
                    &format!("DROP {kind} a {behavior}"),
                    "public.d",
                    "COMMIT",
                );
                result.unwrap();
                assert_eq!(
                    sql(&second, "SELECT to_regclass('a') AS relation").rows[0]["relation"],
                    Value::Null
                );
                assert_eq!(sql(&second, "SELECT * FROM d").rows[0]["v"], Value::Int(2));
                assert_eq!(
                    sql(&second, "SELECT * FROM outer_view").rows[0]["v"],
                    Value::Int(2)
                );
            }
        }
    }
}

#[test]
fn relation_drop_waits_before_restrict_errors_or_transitive_cascade_publication() {
    for provider in 0..3 {
        for (kind, create) in DEPENDENCY_ROOTS {
            for behavior in ["CASCADE", "RESTRICT"] {
                let (_directory, first, second) = sessions(provider);
                create_dependency_chain(&first, create);
                sql(&first, "BEGIN; ALTER VIEW d SET (security_barrier=true)");
                let (second, result) = after_wait(
                    &first,
                    second,
                    &format!("DROP {kind} a {behavior}"),
                    "public.d",
                    "COMMIT",
                );
                if behavior == "RESTRICT" {
                    assert_eq!(result.unwrap_err().sqlstate(), Some("2BP01"));
                    for name in ["a", "d", "outer_view"] {
                        assert_ne!(
                            sql(
                                &second,
                                &format!("SELECT to_regclass('{name}') AS relation")
                            )
                            .rows[0]["relation"],
                            Value::Null
                        );
                    }
                } else {
                    result.unwrap();
                    for name in ["a", "d", "outer_view"] {
                        assert_eq!(
                            sql(
                                &second,
                                &format!("SELECT to_regclass('{name}') AS relation")
                            )
                            .rows[0]["relation"],
                            Value::Null
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn cascading_relation_drop_follows_renamed_views_and_preserves_reused_names() {
    for provider in 0..3 {
        for (kind, create) in DEPENDENCY_ROOTS {
            let (_directory, first, second) = sessions(provider);
            create_dependency_chain(&first, create);
            sql(
                &first,
                "BEGIN; ALTER VIEW d RENAME TO moved; CREATE VIEW d AS SELECT 2 AS v",
            );
            let (second, result) = after_wait(
                &first,
                second,
                &format!("DROP {kind} a CASCADE"),
                "public.d",
                "COMMIT",
            );
            result.unwrap();
            assert_eq!(sql(&second, "SELECT * FROM d").rows[0]["v"], Value::Int(2));
            for name in ["a", "moved", "outer_view"] {
                assert_eq!(
                    sql(
                        &second,
                        &format!("SELECT to_regclass('{name}') AS relation")
                    )
                    .rows[0]["relation"],
                    Value::Null
                );
            }
        }
    }
}

#[test]
fn direct_view_and_inherited_table_drops_recheck_waited_view_dependencies() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        create_dependency_chain(&first, "CREATE VIEW a AS SELECT 1 AS v");
        sql(&first, "BEGIN; CREATE OR REPLACE VIEW d AS SELECT * FROM b");
        let (second, result) =
            after_operation_wait(&first, second, "public.d", "COMMIT", |engine| {
                engine.drop_view("a")
            });
        assert!(result.unwrap());
        assert_eq!(
            sql(&second, "SELECT * FROM outer_view").rows[0]["v"],
            Value::Int(2)
        );

        let (_directory, first, second) = sessions(provider);
        sql(
            &first,
            "CREATE TABLE parent(v integer); CREATE TABLE a() INHERITS(parent)",
        );
        create_dependency_chain(&first, "INSERT INTO a VALUES (1)");
        sql(&first, "BEGIN; CREATE OR REPLACE VIEW d AS SELECT * FROM b");
        let (second, result) = after_wait(
            &first,
            second,
            "DROP TABLE parent CASCADE",
            "public.d",
            "COMMIT",
        );
        result.unwrap();
        assert_eq!(
            sql(&second, "SELECT to_regclass('a') AS relation").rows[0]["relation"],
            Value::Null
        );
        assert_eq!(
            sql(&second, "SELECT * FROM outer_view").rows[0]["v"],
            Value::Int(2)
        );
    }
}

#[test]
fn cancelling_a_dependency_wait_leaves_every_view_and_root_intact() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        create_dependency_chain(&first, "CREATE VIEW a AS SELECT 1 AS v");
        sql(&first, "BEGIN; ALTER VIEW d SET (security_barrier=true)");
        let session = second.session_id;
        let cancel = second.runtime.cancellation.clone();
        let task = thread::spawn(move || second.sql("DROP VIEW a CASCADE", &[]));
        let waited = wait_for_relation(&first, session, "public.d", || task.is_finished());
        cancel.cancel();
        let result = task.join().unwrap();
        sql(&first, "ROLLBACK");
        assert!(waited);
        assert_eq!(result.unwrap_err().sqlstate(), Some("57014"));
        assert_eq!(
            sql(&first, "SELECT * FROM outer_view").rows[0]["v"],
            Value::Int(1)
        );
    }
}

#[test]
fn relation_drop_rechecks_view_dependencies_after_a_fixed_data_snapshot() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            let (_directory, first, second) = sessions(provider);
            create_dependency_chain(&first, "CREATE VIEW a AS SELECT 1 AS v");
            sql(
                &second,
                &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t"),
            );
            sql(&first, "BEGIN; CREATE OR REPLACE VIEW d AS SELECT * FROM b");
            let (second, result) =
                after_wait(&first, second, "DROP VIEW a CASCADE", "public.d", "COMMIT");
            result.unwrap_or_else(|error| panic!("provider {provider}, {isolation}: {error}"));
            assert_eq!(
                sql(&second, "SELECT * FROM outer_view").rows[0]["v"],
                Value::Int(2)
            );
            sql(&second, "COMMIT");
            assert_eq!(
                sql(&first, "SELECT to_regclass('a') AS relation").rows[0]["relation"],
                Value::Null
            );
        }
    }
}
