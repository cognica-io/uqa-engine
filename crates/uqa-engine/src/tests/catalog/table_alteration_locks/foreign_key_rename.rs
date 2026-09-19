//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign-key catalog identities survive local renames, deferred checks and persistent restoration.

use super::{after_wait, error, peer_lock, sessions, sql, Engine, Value};
use crate::tests::relation_lock_support::reopen;

fn oid(engine: &Engine, table: &str, constraint: &str) -> Value {
    sql(engine, &format!("SELECT oid FROM pg_constraint WHERE conrelid='{table}'::regclass AND conname='{constraint}'")).rows[0]["oid"].clone()
}

#[test]
fn foreign_key_identity_survives_column_relation_and_constraint_renames() {
    for provider in 0..3 {
        let (_directory, first, _second) = sessions(provider);
        sql(&first, "CREATE TABLE referenced(id integer PRIMARY KEY); CREATE TABLE inline_table(v integer CONSTRAINT fk REFERENCES referenced(id)); ALTER TABLE t ADD CONSTRAINT fk FOREIGN KEY(v) REFERENCES referenced(id) NOT VALID");
        for table in ["inline_table", "t"] {
            let original = oid(&first, table, "fk");
            sql(&first, &format!("ALTER TABLE {table} RENAME COLUMN v TO value; ALTER TABLE {table} RENAME CONSTRAINT fk TO renamed; ALTER TABLE {table} RENAME TO new_{table}"));
            assert_eq!(oid(&first, &format!("new_{table}"), "renamed"), original);
            error(
                &first,
                &format!("INSERT INTO new_{table} VALUES(20)"),
                "23503",
            );
        }
    }
}

#[test]
fn foreign_key_rename_preserves_savepoint_identity_and_releases_its_relation_lock() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE referenced(id integer PRIMARY KEY); ALTER TABLE t ADD CONSTRAINT fk FOREIGN KEY(v) REFERENCES referenced(id) NOT VALID");
        let original = oid(&first, "t", "fk");
        sql(
            &first,
            "BEGIN; SAVEPOINT before_rename; ALTER TABLE t RENAME CONSTRAINT fk TO renamed",
        );
        assert_eq!(oid(&first, "t", "renamed"), original);
        peer_lock(&second, "t", "ACCESS SHARE", false);
        peer_lock(&second, "referenced", "ACCESS EXCLUSIVE", true);
        sql(&first, "ROLLBACK TO before_rename");
        assert_eq!(oid(&first, "t", "fk"), original);
        peer_lock(&second, "t", "ACCESS EXCLUSIVE", true);
        sql(
            &first,
            "COMMIT; ALTER TABLE t ADD CONSTRAINT occupied CHECK(v>0)",
        );
        error(
            &first,
            "ALTER TABLE t RENAME CONSTRAINT fk TO occupied",
            "42710",
        );
        assert_eq!(oid(&first, "t", "fk"), original);
        sql(&first, "ALTER TABLE t DROP CONSTRAINT fk; ALTER TABLE t ADD CONSTRAINT fk FOREIGN KEY(v) REFERENCES referenced(id) NOT VALID");
        assert_ne!(oid(&first, "t", "fk"), original);
    }
}

#[test]
fn partition_foreign_key_renames_are_local_and_preserve_distinct_catalog_rows() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE referenced(id integer PRIMARY KEY); CREATE TABLE parent(v integer, CONSTRAINT fk FOREIGN KEY(v) REFERENCES referenced(id)) PARTITION BY RANGE(v); CREATE TABLE child PARTITION OF parent FOR VALUES FROM(0) TO(10)");
        let parent = oid(&first, "parent", "fk");
        let child = oid(&first, "child", "fk");
        assert_ne!(parent, child);
        sql(
            &first,
            "BEGIN; ALTER TABLE ONLY parent RENAME CONSTRAINT fk TO parent_fk",
        );
        assert_eq!(oid(&first, "parent", "parent_fk"), parent);
        assert_eq!(oid(&first, "child", "fk"), child);
        peer_lock(&second, "parent", "ACCESS SHARE", false);
        peer_lock(&second, "child", "ACCESS EXCLUSIVE", true);
        peer_lock(&second, "referenced", "ACCESS EXCLUSIVE", true);
        sql(
            &first,
            "COMMIT; ALTER TABLE child RENAME CONSTRAINT fk TO child_fk",
        );
        assert_eq!(oid(&first, "parent", "parent_fk"), parent);
        assert_eq!(oid(&first, "child", "child_fk"), child);
        sql(&first, "ALTER TABLE parent DROP CONSTRAINT parent_fk");
        assert_eq!(
            sql(
                &first,
                "SELECT count(*) AS n FROM pg_constraint WHERE contype='f'"
            )
            .rows[0]["n"],
            Value::Int(0)
        );
    }
}

#[test]
fn renamed_foreign_key_keeps_deferred_mode_and_pending_events() {
    for provider in 0..3 {
        let (_directory, first, _second) = sessions(provider);
        sql(&first, "CREATE TABLE referenced(id integer PRIMARY KEY); CREATE TABLE child(v integer CONSTRAINT fk REFERENCES referenced(id) DEFERRABLE)");
        let original = oid(&first, "child", "fk");
        sql(&first, "BEGIN; SET CONSTRAINTS fk DEFERRED; INSERT INTO child VALUES(9); ALTER TABLE child RENAME CONSTRAINT fk TO renamed");
        assert_eq!(oid(&first, "child", "renamed"), original);
        error(&first, "SET CONSTRAINTS renamed IMMEDIATE", "23503");
        sql(&first, "ROLLBACK; BEGIN; SET CONSTRAINTS fk DEFERRED; INSERT INTO child VALUES(9); ALTER TABLE child RENAME CONSTRAINT fk TO renamed; INSERT INTO child VALUES(10); INSERT INTO referenced VALUES(9),(10); COMMIT");
        assert_eq!(oid(&first, "child", "renamed"), original);
        assert_eq!(
            sql(&first, "SELECT count(*) AS n FROM child").rows[0]["n"],
            Value::Int(2)
        );
        error(&first, "INSERT INTO child VALUES(11)", "23503");
    }
}

#[test]
fn foreign_key_rename_rebinds_a_reused_table_name_after_a_definition_wait() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE referenced(id integer PRIMARY KEY); ALTER TABLE t ADD CONSTRAINT fk FOREIGN KEY(v) REFERENCES referenced(id) NOT VALID");
        let original = oid(&first, "t", "fk");
        sql(&first, "BEGIN; ALTER TABLE t RENAME TO original; CREATE TABLE t(v integer CONSTRAINT fk REFERENCES referenced(id))");
        let replacement = oid(&first, "t", "fk");
        let (second, result) = after_wait(
            &first,
            second,
            "BEGIN; ALTER TABLE t RENAME CONSTRAINT fk TO renamed",
            "public.t",
            "COMMIT",
        );
        result.unwrap();
        assert_eq!(oid(&second, "original", "fk"), original);
        assert_eq!(oid(&second, "t", "renamed"), replacement);
        peer_lock(&first, "original", "ACCESS EXCLUSIVE", true);
        peer_lock(&first, "t", "ACCESS SHARE", false);
        sql(&second, "ROLLBACK");
        assert_eq!(oid(&second, "original", "fk"), original);
        assert_eq!(oid(&second, "t", "fk"), replacement);
    }
}

#[test]
fn attached_partition_foreign_key_provenance_and_oids_survive_rename_and_reopen() {
    for provider in 0..3 {
        let (directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE referenced(id integer PRIMARY KEY); INSERT INTO referenced VALUES(1); CREATE TABLE parent(v integer, CONSTRAINT fk FOREIGN KEY(v) REFERENCES referenced(id)) PARTITION BY RANGE(v); ALTER TABLE parent ATTACH PARTITION t FOR VALUES FROM(0) TO(10)");
        let parent = oid(&first, "parent", "fk");
        let child = oid(&first, "t", "fk");
        assert_ne!(parent, child);
        sql(&first, "ALTER TABLE t RENAME CONSTRAINT fk TO child_fk; ALTER TABLE parent RENAME CONSTRAINT fk TO parent_fk");
        drop(second);
        drop(first);
        let restored = reopen(provider, &directory.path().join("table-locks.db"));
        assert_eq!(oid(&restored, "parent", "parent_fk"), parent);
        assert_eq!(oid(&restored, "t", "child_fk"), child);
        error(&restored, "INSERT INTO t VALUES(2)", "23503");
        sql(&restored, "ALTER TABLE parent DROP CONSTRAINT parent_fk");
        sql(
            &restored,
            "ALTER TABLE t ADD CONSTRAINT required NOT NULL v",
        );
        assert_eq!(
            sql(
                &restored,
                "SELECT count(*) AS n FROM pg_constraint WHERE contype='f'"
            )
            .rows[0]["n"],
            Value::Int(0)
        );
    }
}

mod restoration;

#[test]
fn only_partitions_inherit_foreign_key_rows_when_creating_or_extending_columns() {
    for provider in 0..3 {
        let (_directory, first, _second) = sessions(provider);
        sql(&first, "CREATE TABLE referenced(id integer PRIMARY KEY); CREATE TABLE ordinary(v integer CONSTRAINT existing REFERENCES referenced(id)); CREATE TABLE inherited() INHERITS(ordinary); ALTER TABLE ordinary ADD COLUMN ref integer CONSTRAINT added REFERENCES referenced(id)");
        assert_eq!(sql(&first, "SELECT count(*) AS n FROM pg_constraint WHERE conrelid='inherited'::regclass AND contype='f'").rows[0]["n"], Value::Int(0));
        sql(&first, "INSERT INTO inherited VALUES(50,60); CREATE TABLE parent(v integer) PARTITION BY RANGE(v); CREATE TABLE child PARTITION OF parent FOR VALUES FROM(0) TO(10); ALTER TABLE parent ADD COLUMN ref integer CONSTRAINT added REFERENCES referenced(id)");
        assert_ne!(
            oid(&first, "parent", "added"),
            oid(&first, "child", "added")
        );
        sql(
            &first,
            "ALTER TABLE child RENAME CONSTRAINT added TO child_fk",
        );
        error(&first, "INSERT INTO child VALUES(1,60)", "23503");
        first.new_session().unwrap();
    }
}
