//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! View definition, read and refresh locks across persistent provider sessions.

use super::{
    relation_lock_support::{after_wait, error, sessions, sql},
    *,
};
use std::{sync::mpsc, thread, time::Duration};

#[test]
fn view_reads_retain_definition_locks_until_transaction_or_savepoint_end() {
    for provider in 0..3 {
        for (read, change, relation) in [
            (
                "SELECT * FROM v",
                "CREATE OR REPLACE VIEW v AS SELECT 2 AS v",
                "public.v",
            ),
            (
                "SELECT * FROM v",
                "ALTER VIEW v SET (security_barrier=true)",
                "public.v",
            ),
            ("SELECT * FROM v", "DROP VIEW v", "public.v"),
            ("SELECT * FROM m", "REFRESH MATERIALIZED VIEW m", "public.m"),
            (
                "SELECT * FROM m",
                "REFRESH MATERIALIZED VIEW m WITH NO DATA",
                "public.m",
            ),
            ("SELECT * FROM m", "DROP MATERIALIZED VIEW m", "public.m"),
        ] {
            let (_directory, first, second) = sessions(provider);
            sql(
                &first,
                "CREATE VIEW v AS SELECT 1 AS v; CREATE MATERIALIZED VIEW m AS SELECT 1 AS v",
            );
            sql(&first, "BEGIN; SAVEPOINT before_read");
            sql(&first, read);
            let (_, result) =
                after_wait(&first, second, change, relation, "ROLLBACK TO before_read");
            result.unwrap();
            sql(&first, "COMMIT");
        }
    }
}

#[test]
fn view_read_and_alter_rebind_the_definition_after_replacement_commits() {
    for provider in 0..3 {
        for statement in [
            "SELECT * FROM v",
            "ALTER VIEW v SET (security_barrier=true)",
        ] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE VIEW v AS SELECT 1 AS v");
            sql(&first, "BEGIN; CREATE OR REPLACE VIEW v AS SELECT 2 AS v");
            let (second, result) = after_wait(&first, second, statement, "public.v", "COMMIT");
            let result = result.unwrap();
            if statement.starts_with("SELECT") {
                assert_eq!(result.rows[0]["v"], Value::Int(2));
            }
            assert_eq!(sql(&second, "SELECT * FROM v").rows[0]["v"], Value::Int(2));
            if statement.starts_with("ALTER") {
                assert!(second
                    .view_definition("v")
                    .unwrap()
                    .unwrap()
                    .options
                    .contains(&("security_barrier".into(), "true".into())));
            }
        }
    }
}

#[test]
fn view_changes_recheck_ownership_after_waiting_on_the_same_identity() {
    for provider in 0..3 {
        for (kind, statement) in [
            ("VIEW", "ALTER VIEW v SET (security_barrier=true)"),
            ("VIEW", "CREATE OR REPLACE VIEW v AS SELECT 2 AS v"),
            ("MATERIALIZED VIEW", "REFRESH MATERIALIZED VIEW v"),
        ] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE ROLE before_owner; CREATE ROLE after_owner; GRANT CREATE ON SCHEMA public TO before_owner, after_owner");
            sql(
                &first,
                &format!("CREATE {kind} v AS SELECT 1 AS v; ALTER {kind} v OWNER TO before_owner"),
            );
            sql(&second, "SET ROLE before_owner");
            sql(
                &first,
                &format!("BEGIN; ALTER {kind} v OWNER TO after_owner"),
            );
            let (_, result) = after_wait(&first, second, statement, "public.v", "COMMIT");
            assert_eq!(result.unwrap_err().sqlstate(), Some("42501"));
            assert_eq!(
                first.view_definition("v").unwrap().unwrap().role_owner,
                "after_owner"
            );
            assert_eq!(sql(&first, "SELECT * FROM v").rows[0]["v"], Value::Int(1));
        }
    }
}

#[test]
fn view_creation_retains_source_locks_including_unpopulated_materialized_views() {
    for provider in 0..3 {
        for create in [
            "CREATE VIEW v AS SELECT * FROM t",
            "CREATE MATERIALIZED VIEW v AS SELECT * FROM t",
            "CREATE MATERIALIZED VIEW v AS SELECT * FROM t WITH NO DATA",
        ] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "BEGIN");
            sql(&first, create);
            let (_, result) = after_wait(
                &first,
                second,
                "ALTER TABLE t ADD COLUMN extra integer",
                "public.t",
                "COMMIT",
            );
            result.unwrap();
        }
    }
}

#[test]
fn concurrent_view_creation_rechecks_a_previously_absent_target() {
    for provider in 0..3 {
        for (initial, next, expected) in [
            (
                "CREATE VIEW v AS SELECT 1 AS v",
                "CREATE VIEW v AS SELECT 2 AS v",
                Some("42P07"),
            ),
            (
                "CREATE VIEW v AS SELECT 1 AS v",
                "CREATE OR REPLACE VIEW v AS SELECT 2 AS v",
                None,
            ),
            (
                "CREATE MATERIALIZED VIEW v AS SELECT 1 AS v",
                "CREATE MATERIALIZED VIEW IF NOT EXISTS v AS SELECT 2 AS v",
                None,
            ),
        ] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "BEGIN");
            sql(&first, initial);
            let (second, result) = after_wait(&first, second, next, "public.v", "COMMIT");
            if let Some(expected) = expected {
                assert_eq!(result.unwrap_err().sqlstate(), Some(expected));
            } else {
                result.unwrap();
                let expected = if next.starts_with("CREATE OR REPLACE") {
                    2
                } else {
                    1
                };
                assert_eq!(
                    sql(&second, "SELECT * FROM v").rows[0]["v"],
                    Value::Int(expected)
                );
            }
        }
    }
}

#[test]
fn view_refresh_readers_observe_committed_rows_after_waiting() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(
            &first,
            "CREATE MATERIALIZED VIEW m AS SELECT * FROM t; INSERT INTO t VALUES (2)",
        );
        sql(&first, "BEGIN; REFRESH MATERIALIZED VIEW m");
        let (_, result) = after_wait(
            &first,
            second,
            "SELECT * FROM m ORDER BY v",
            "public.m",
            "COMMIT",
        );
        let rows = result.unwrap().rows;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1]["v"], Value::Int(2));
    }
}

#[test]
fn altered_view_locks_and_definitions_restore_at_savepoints() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE VIEW v AS SELECT 1 AS v");
        sql(
            &first,
            "BEGIN; SAVEPOINT before_change; CREATE OR REPLACE VIEW v AS SELECT 2 AS v",
        );
        sql(&second, "BEGIN");
        error(&second, "LOCK v IN ACCESS SHARE MODE NOWAIT", "55P03");
        sql(&second, "ROLLBACK");
        sql(&first, "ROLLBACK TO before_change");
        sql(&second, "BEGIN; LOCK v IN ACCESS SHARE MODE NOWAIT; COMMIT");
        assert_eq!(sql(&first, "SELECT * FROM v").rows[0]["v"], Value::Int(1));
        sql(&first, "COMMIT");
    }
}

#[test]
fn distinct_view_definitions_can_change_in_overlapping_transactions() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(
            &first,
            "CREATE VIEW a AS SELECT 1 AS v; CREATE VIEW b AS SELECT 1 AS v",
        );
        sql(&first, "BEGIN; CREATE OR REPLACE VIEW a AS SELECT 2 AS v");
        sql(
            &second,
            "BEGIN; CREATE OR REPLACE VIEW b AS SELECT 3 AS v; COMMIT",
        );
        sql(&first, "COMMIT");
        assert_eq!(sql(&first, "SELECT * FROM a").rows[0]["v"], Value::Int(2));
        assert_eq!(sql(&second, "SELECT * FROM b").rows[0]["v"], Value::Int(3));
    }
}

#[test]
fn view_creation_rebinds_sources_changed_while_acquiring_their_read_locks() {
    for provider in 0..3 {
        for replace in [false, true] {
            let (_directory, first, second) = sessions(provider);
            if replace {
                sql(
                    &first,
                    "BEGIN; DROP TABLE t; CREATE TABLE t(v integer); INSERT INTO t VALUES (2)",
                );
            } else {
                sql(&first, "BEGIN; ALTER TABLE t RENAME TO renamed_t");
            }
            let (second, result) = after_wait(
                &first,
                second,
                "CREATE VIEW v AS SELECT * FROM t",
                "public.t",
                "COMMIT",
            );
            if replace {
                result.unwrap();
                assert_eq!(sql(&second, "SELECT * FROM v").rows[0]["v"], Value::Int(2));
            } else {
                assert_eq!(result.unwrap_err().sqlstate(), Some("42P01"));
                assert!(second.view_definition("v").unwrap().is_none());
            }
        }
    }
}

#[test]
fn alter_view_if_exists_does_not_restore_a_concurrently_dropped_definition() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE VIEW v AS SELECT 1 AS v");
        sql(&first, "BEGIN; DROP VIEW v");
        let (second, result) = after_wait(
            &first,
            second,
            "ALTER VIEW IF EXISTS v SET (security_barrier=true)",
            "public.v",
            "COMMIT",
        );
        result.unwrap();
        assert!(second.view_definition("v").unwrap().is_none());
    }
}

#[test]
fn materialized_view_options_allow_reads_and_exclude_other_maintenance() {
    use uqa_execution::row_locks::RelationLockMode;
    for provider in 0..3 {
        for statement in [
            "ALTER MATERIALIZED VIEW m SET (fillfactor=80)",
            "ALTER MATERIALIZED VIEW m RESET (fillfactor)",
        ] {
            let (_directory, first, second) = sessions(provider);
            sql(
                &first,
                "CREATE MATERIALIZED VIEW m WITH (fillfactor=90) AS SELECT 1 AS v",
            );
            sql(&first, "BEGIN; SELECT * FROM m");
            let statement = statement.to_string();
            let cancel = second.runtime.cancellation.clone();
            let (send, done) = mpsc::channel();
            let task = thread::spawn(move || {
                sql(&second, "BEGIN");
                let result = second.sql(&statement, &[]);
                let _ = send.send(result);
                second
            });
            let result = done.recv_timeout(Duration::from_secs(30));
            if result.is_err() {
                cancel.cancel();
            }
            let second = task.join().unwrap();
            result
                .expect("fillfactor change must finish while the reader remains open")
                .unwrap();
            let relation = first.row_locks.table_key("public.m");
            for (mode, allowed) in [
                (RelationLockMode::ShareUpdateExclusive, false),
                (RelationLockMode::RowExclusive, true),
            ] {
                assert_eq!(
                    first
                        .row_locks
                        .try_acquire_relation(
                            first.session_id,
                            relation,
                            mode,
                            0,
                            &first.runtime.cancellation,
                        )
                        .unwrap(),
                    allowed
                );
            }
            sql(&second, "COMMIT");
            assert_eq!(sql(&first, "SELECT * FROM m").rows[0]["v"], Value::Int(1));
            sql(&first, "COMMIT");
        }
    }
}
