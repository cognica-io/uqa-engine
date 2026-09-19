//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign-key removal retains reference and dependent relation identities through waits.

use super::{after_wait, peer_lock, sessions, sql, Engine, Value};

fn has_foreign_key(engine: &Engine, table: &str) -> bool {
    sql(engine, &format!("SELECT count(*) AS n FROM pg_constraint WHERE contype='f' AND conrelid='{table}'::regclass")).rows[0]["n"] != Value::Int(0)
}

fn foreign_key(engine: &Engine) {
    sql(engine, "CREATE TABLE p(id integer CONSTRAINT pk PRIMARY KEY); INSERT INTO p VALUES(1); ALTER TABLE t ADD CONSTRAINT fk FOREIGN KEY(v) REFERENCES p(id)");
}

#[test]
fn foreign_key_removal_retains_reference_locks_and_releases_them_at_savepoints() {
    for provider in 0..3 {
        for enforced in [true, false] {
            let (_directory, first, second) = sessions(provider);
            foreign_key(&first);
            if !enforced {
                sql(&first, "ALTER TABLE t ALTER CONSTRAINT fk NOT ENFORCED");
            }
            sql(
                &first,
                "BEGIN; SAVEPOINT before_drop; ALTER TABLE t DROP CONSTRAINT fk",
            );
            assert!(!has_foreign_key(&first, "t"));
            peer_lock(&second, "p", "ACCESS SHARE", false);
            sql(&first, "ROLLBACK TO before_drop");
            assert!(has_foreign_key(&first, "t"));
            peer_lock(&second, "p", "ACCESS EXCLUSIVE", true);
            sql(&first, "ROLLBACK");
        }
    }
}

#[test]
fn foreign_key_removal_waits_for_the_original_reference_after_name_reuse() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        foreign_key(&first);
        sql(
            &first,
            "BEGIN; ALTER TABLE p RENAME TO original; CREATE TABLE p(id integer PRIMARY KEY)",
        );
        let (second, result) = after_wait(
            &first,
            second,
            "BEGIN; ALTER TABLE t DROP CONSTRAINT fk",
            "public.p",
            "COMMIT",
        );
        result.unwrap();
        assert!(!has_foreign_key(&second, "t"));
        peer_lock(&first, "original", "ACCESS SHARE", false);
        peer_lock(&first, "p", "ACCESS EXCLUSIVE", true);
        sql(&second, "ROLLBACK");
        assert!(has_foreign_key(&second, "t"));
        sql(&second, "INSERT INTO t VALUES(1)");
    }
}

#[test]
fn key_column_and_index_cascades_retain_referrer_locks() {
    for provider in 0..3 {
        for action in [
            "ALTER TABLE p DROP CONSTRAINT pk CASCADE",
            "ALTER TABLE p DROP COLUMN id CASCADE",
            "DROP INDEX parent_key CASCADE",
        ] {
            let (_directory, first, second) = sessions(provider);
            if action.starts_with("DROP INDEX") {
                sql(&first, "CREATE TABLE p(id integer); CREATE UNIQUE INDEX parent_key ON p(id); INSERT INTO p VALUES(1); ALTER TABLE t ADD CONSTRAINT fk FOREIGN KEY(v) REFERENCES p(id)");
            } else {
                foreign_key(&first);
            }
            sql(&first, &format!("BEGIN; SAVEPOINT before_drop; {action}"));
            assert!(!has_foreign_key(&first, "t"));
            peer_lock(&second, "t", "ACCESS SHARE", false);
            sql(&first, "ROLLBACK TO before_drop");
            assert!(has_foreign_key(&first, "t"));
            peer_lock(&second, "t", "ACCESS EXCLUSIVE", true);
            sql(&first, "ROLLBACK");
        }
    }
}

#[test]
fn cascades_follow_referrers_renamed_during_a_definition_wait() {
    for provider in 0..3 {
        for action in [
            "ALTER TABLE p DROP CONSTRAINT pk CASCADE",
            "ALTER TABLE p DROP COLUMN id CASCADE",
            "DROP INDEX parent_key CASCADE",
        ] {
            let (_directory, first, second) = sessions(provider);
            if action.starts_with("DROP INDEX") {
                sql(&first, "CREATE TABLE p(id integer); CREATE UNIQUE INDEX parent_key ON p(id); INSERT INTO p VALUES(1); ALTER TABLE t ADD CONSTRAINT fk FOREIGN KEY(v) REFERENCES p(id)");
            } else {
                foreign_key(&first);
            }
            sql(&first, "BEGIN; ALTER TABLE t RENAME TO original; CREATE TABLE t(v integer CONSTRAINT unrelated CHECK(v>0))");
            let (second, result) = after_wait(
                &first,
                second,
                &format!("BEGIN; {action}"),
                "public.t",
                "COMMIT",
            );
            result.unwrap();
            assert!(!has_foreign_key(&second, "original"));
            peer_lock(&first, "original", "ACCESS SHARE", false);
            peer_lock(&first, "t", "ACCESS EXCLUSIVE", true);
            assert_eq!(sql(&second, "SELECT count(*) AS n FROM pg_constraint WHERE conrelid='t'::regclass AND conname='unrelated'").rows[0]["n"], Value::Int(1));
            sql(&second, "ROLLBACK");
            assert!(has_foreign_key(&second, "original"));
        }
    }
}

#[test]
fn partition_foreign_key_removal_locks_each_referencing_partition() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE p(id integer PRIMARY KEY); CREATE TABLE parent(v integer, CONSTRAINT fk FOREIGN KEY(v) REFERENCES p(id)) PARTITION BY RANGE(v); CREATE TABLE child PARTITION OF parent FOR VALUES FROM(0) TO(10)");
        sql(&first, "BEGIN; ALTER TABLE parent DROP CONSTRAINT fk");
        assert!(!has_foreign_key(&first, "parent"));
        assert!(!has_foreign_key(&first, "child"));
        peer_lock(&second, "child", "ACCESS SHARE", false);
        peer_lock(&second, "p", "ACCESS SHARE", false);
        sql(&first, "ROLLBACK");
    }
}

#[test]
fn self_referencing_foreign_key_removal_keeps_the_primary_relation_lock() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "ALTER TABLE t ADD CONSTRAINT pk PRIMARY KEY(v); ALTER TABLE t ADD CONSTRAINT fk FOREIGN KEY(v) REFERENCES t(v)");
        sql(&first, "BEGIN; ALTER TABLE t DROP CONSTRAINT fk");
        assert!(!has_foreign_key(&first, "t"));
        peer_lock(&second, "t", "ACCESS SHARE", false);
        sql(&first, "ROLLBACK");
        assert!(has_foreign_key(&first, "t"));
    }
}

#[test]
fn cascades_capture_every_referrer_before_the_first_dependency_wait() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        foreign_key(&first);
        sql(
            &first,
            "CREATE TABLE u(v integer CONSTRAINT fk REFERENCES p(id))",
        );
        sql(&first, "BEGIN; ALTER TABLE t RENAME TO original_t; ALTER TABLE u RENAME TO original_u; CREATE TABLE t(v integer CONSTRAINT fk CHECK(v>0)); CREATE TABLE u(v integer CONSTRAINT fk CHECK(v>0))");
        let (second, result) = after_wait(
            &first,
            second,
            "BEGIN; ALTER TABLE p DROP CONSTRAINT pk CASCADE",
            "public.t",
            "COMMIT",
        );
        result.unwrap();
        for table in ["original_t", "original_u"] {
            assert!(!has_foreign_key(&second, table));
            peer_lock(&first, table, "ACCESS SHARE", false);
        }
        for table in ["t", "u"] {
            assert_eq!(sql(&second, &format!("SELECT count(*) AS n FROM pg_constraint WHERE conrelid='{table}'::regclass AND conname='fk' AND contype='c'")).rows[0]["n"], Value::Int(1));
            peer_lock(&first, table, "ACCESS EXCLUSIVE", true);
        }
        sql(&second, "ROLLBACK");
    }
}

#[test]
fn key_removal_preserves_metadata_refreshed_while_a_dependent_lock_waits() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        foreign_key(&first);
        sql(&first, "CREATE TABLE q(id integer PRIMARY KEY); INSERT INTO q VALUES(1); ALTER TABLE p ADD COLUMN qid integer; ALTER TABLE p ADD CONSTRAINT retained_fk FOREIGN KEY(qid) REFERENCES q(id)");
        sql(&first, "BEGIN; ALTER TABLE t RENAME TO original_t; ALTER TABLE q RENAME TO original_q; CREATE TABLE q(id integer PRIMARY KEY)");
        let (second, result) = after_wait(
            &first,
            second,
            "BEGIN; ALTER TABLE p DROP CONSTRAINT pk CASCADE",
            "public.t",
            "COMMIT",
        );
        result.unwrap();
        assert!(has_foreign_key(&second, "p"));
        sql(&second, "INSERT INTO p VALUES(2,1); COMMIT");
        sql(&first, "INSERT INTO p VALUES(3,1)");
    }
}

#[test]
fn index_cascade_preserves_a_foreign_key_replaced_during_the_parent_lock_wait() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE p(id integer); CREATE UNIQUE INDEX parent_key ON p(id); INSERT INTO p VALUES(1); CREATE TABLE q(id integer PRIMARY KEY); INSERT INTO q VALUES(1); ALTER TABLE t ADD CONSTRAINT fk FOREIGN KEY(v) REFERENCES p(id)");
        sql(&first, "BEGIN; ALTER TABLE t DROP CONSTRAINT fk; ALTER TABLE t ADD CONSTRAINT fk FOREIGN KEY(v) REFERENCES q(id)");
        let (second, result) = after_wait(
            &first,
            second,
            "BEGIN; DROP INDEX parent_key CASCADE",
            "public.p",
            "COMMIT",
        );
        result.unwrap();
        assert!(has_foreign_key(&second, "t"));
        sql(&second, "INSERT INTO t VALUES(1); COMMIT");
        let error = second.sql("INSERT INTO t VALUES(2)", &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("23503"));
        peer_lock(&first, "q", "ACCESS EXCLUSIVE", true);
    }
}

#[test]
fn index_dependencies_are_refreshed_after_the_parent_lock_wait() {
    for provider in 0..3 {
        for initially_present in [false, true] {
            for cascade in [false, true] {
                let (_directory, first, second) = sessions(provider);
                sql(&first, "CREATE TABLE p(id integer); CREATE UNIQUE INDEX parent_key ON p(id); INSERT INTO p VALUES(1)");
                let addition = "ALTER TABLE t ADD CONSTRAINT fk FOREIGN KEY(v) REFERENCES p(id)";
                if initially_present {
                    sql(&first, addition);
                }
                sql(&first, "BEGIN");
                sql(
                    &first,
                    if initially_present {
                        "ALTER TABLE t DROP CONSTRAINT fk"
                    } else {
                        addition
                    },
                );
                let action = if cascade {
                    "BEGIN; DROP INDEX parent_key CASCADE"
                } else {
                    "BEGIN; DROP INDEX parent_key RESTRICT"
                };
                let (second, result) = after_wait(&first, second, action, "public.p", "COMMIT");
                if !initially_present && !cascade {
                    assert_eq!(result.unwrap_err().sqlstate(), Some("2BP01"));
                    sql(&second, "ROLLBACK");
                    assert!(has_foreign_key(&second, "t"));
                } else {
                    result.unwrap();
                    assert!(!has_foreign_key(&second, "t"));
                    sql(&second, "COMMIT");
                }
            }
        }
    }
}
