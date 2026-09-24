//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Constraint and inheritance validation retain every relation they inspect.

use super::{after_wait, error, peer_lock, sessions, sql, Engine, Value};

fn not_null_validated(engine: &Engine, table: &str) -> bool {
    let result = sql(engine, &format!("SELECT convalidated FROM pg_constraint WHERE contype='n' AND conrelid='{table}'::regclass"));
    assert_eq!(result.rows.len(), 1, "{table}");
    result.rows[0]["convalidated"] == Value::Bool(true)
}

#[test]
fn foreign_key_validation_retains_row_share_only_when_validation_is_needed() {
    for provider in 0..3 {
        for validated in [false, true] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE TABLE p(id integer PRIMARY KEY); INSERT INTO p VALUES(1); ALTER TABLE t ADD CONSTRAINT fk FOREIGN KEY(v) REFERENCES p(id) NOT VALID");
            if validated {
                sql(&first, "ALTER TABLE t VALIDATE CONSTRAINT fk");
            }
            sql(
                &first,
                "BEGIN; SAVEPOINT before_validation; ALTER TABLE t VALIDATE CONSTRAINT fk",
            );
            peer_lock(&second, "p", "SHARE ROW EXCLUSIVE", true);
            peer_lock(&second, "p", "EXCLUSIVE", validated);
            sql(&first, "ROLLBACK TO before_validation");
            peer_lock(&second, "p", "ACCESS EXCLUSIVE", true);
            sql(&first, "ROLLBACK");
        }
    }
}

#[test]
fn not_null_validation_locks_all_descendants_and_releases_them_at_savepoints() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE child() INHERITS(t); CREATE TABLE grandchild() INHERITS(child); ALTER TABLE t ADD CONSTRAINT required NOT NULL v NOT VALID");
        sql(
            &first,
            "BEGIN; SAVEPOINT before_validation; ALTER TABLE t VALIDATE CONSTRAINT required",
        );
        for table in ["t", "child", "grandchild"] {
            peer_lock(&second, table, "ROW EXCLUSIVE", true);
            peer_lock(&second, table, "SHARE UPDATE EXCLUSIVE", false);
            assert!(not_null_validated(&first, table), "{provider}/{table}");
        }
        sql(&first, "ROLLBACK TO before_validation");
        for table in ["t", "child", "grandchild"] {
            peer_lock(&second, table, "ACCESS EXCLUSIVE", true);
            assert!(!not_null_validated(&first, table), "{provider}/{table}");
        }
        sql(&first, "ROLLBACK");
    }
}

#[test]
fn not_null_validation_matches_inherited_constraints_by_column() {
    for provider in 0..3 {
        let (_directory, first, _second) = sessions(provider);
        sql(&first, "CREATE TABLE child() INHERITS(t); CREATE TABLE grandchild() INHERITS(child); ALTER TABLE grandchild ADD CONSTRAINT grandchild_nn NOT NULL v NOT VALID; ALTER TABLE child ADD CONSTRAINT child_nn NOT NULL v NOT VALID; ALTER TABLE t ADD CONSTRAINT root_nn NOT NULL v NOT VALID");
        sql(&first, "ALTER TABLE t VALIDATE CONSTRAINT root_nn");
        for table in ["t", "child", "grandchild"] {
            assert!(not_null_validated(&first, table), "{provider}/{table}");
        }
        let names = sql(&first, "SELECT conname FROM pg_constraint WHERE contype='n' AND conrelid IN ('t'::regclass,'child'::regclass,'grandchild'::regclass) ORDER BY conname");
        assert_eq!(
            names
                .rows
                .iter()
                .map(|row| row["conname"].clone())
                .collect::<Vec<_>>(),
            [
                Value::Str("child_nn".into()),
                Value::Str("grandchild_nn".into()),
                Value::Str("root_nn".into())
            ]
        );
    }
}

#[test]
fn not_null_validation_rejects_nulls_in_children_without_marking_the_parent() {
    for provider in 0..3 {
        let (_directory, first, _second) = sessions(provider);
        sql(&first, "CREATE TABLE child() INHERITS(t); INSERT INTO child VALUES(NULL); ALTER TABLE t ADD CONSTRAINT required NOT NULL v NOT VALID");
        error(
            &first,
            "ALTER TABLE t VALIDATE CONSTRAINT required",
            "23502",
        );
        assert!(!not_null_validated(&first, "t"));
        assert!(!not_null_validated(&first, "child"));
    }
}

#[test]
fn only_not_null_validation_requires_inherited_children_to_be_validated() {
    for provider in 0..3 {
        let (_directory, first, _second) = sessions(provider);
        sql(&first, "CREATE TABLE child() INHERITS(t); ALTER TABLE t ADD CONSTRAINT required NOT NULL v NOT VALID");
        error(
            &first,
            "ALTER TABLE ONLY t VALIDATE CONSTRAINT required",
            "42P16",
        );
        assert!(!not_null_validated(&first, "t"));
        assert!(!not_null_validated(&first, "child"));
    }
}

#[test]
fn inheritance_validation_retains_access_share_on_existing_child_descendants() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE p(v integer); CREATE TABLE child() INHERITS(t); CREATE TABLE grandchild() INHERITS(child)");
        sql(&first, "BEGIN; ALTER TABLE t INHERIT p");
        for table in ["child", "grandchild"] {
            peer_lock(&second, table, "EXCLUSIVE", true);
            peer_lock(&second, table, "ACCESS EXCLUSIVE", false);
        }
        sql(&first, "ROLLBACK");
    }
}

#[test]
fn validation_skips_children_for_noninherited_or_already_validated_not_null() {
    for provider in 0..3 {
        for no_inherit in [false, true] {
            let (_directory, first, second) = sessions(provider);
            let suffix = if no_inherit { " NO INHERIT" } else { "" };
            sql(&first, &format!("CREATE TABLE child() INHERITS(t); ALTER TABLE t ADD CONSTRAINT required NOT NULL v NOT VALID{suffix}"));
            if !no_inherit {
                sql(&first, "ALTER TABLE t VALIDATE CONSTRAINT required");
            }
            sql(
                &first,
                "BEGIN; ALTER TABLE ONLY t VALIDATE CONSTRAINT required",
            );
            peer_lock(&second, "child", "ACCESS EXCLUSIVE", true);
            peer_lock(&second, "t", "ROW EXCLUSIVE", true);
            peer_lock(&second, "t", "SHARE UPDATE EXCLUSIVE", false);
            sql(&first, "ROLLBACK");
        }
    }
}

#[test]
fn foreign_key_validation_waits_for_its_original_reference_through_name_reuse() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE p(id integer PRIMARY KEY); INSERT INTO p VALUES(1); ALTER TABLE t ADD CONSTRAINT fk FOREIGN KEY(v) REFERENCES p(id) NOT VALID");
        sql(
            &first,
            "BEGIN; ALTER TABLE p RENAME TO original; CREATE TABLE p(id integer PRIMARY KEY)",
        );
        let (second, result) = after_wait(
            &first,
            second,
            "BEGIN; ALTER TABLE t VALIDATE CONSTRAINT fk",
            "public.p",
            "COMMIT",
        );
        result.unwrap();
        peer_lock(&first, "original", "EXCLUSIVE", false);
        peer_lock(&first, "p", "ACCESS EXCLUSIVE", true);
        sql(&second, "ROLLBACK");
    }
}

#[test]
fn inherited_not_null_validation_waits_for_the_original_child_through_name_reuse() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE child() INHERITS(t); ALTER TABLE t ADD CONSTRAINT required NOT NULL v NOT VALID");
        sql(&first, "BEGIN; ALTER TABLE child RENAME TO original; CREATE TABLE child(v integer); INSERT INTO child VALUES(NULL)");
        let (second, result) = after_wait(
            &first,
            second,
            "BEGIN; ALTER TABLE t VALIDATE CONSTRAINT required",
            "public.child",
            "COMMIT",
        );
        result.unwrap();
        assert!(not_null_validated(&second, "t"));
        assert!(not_null_validated(&second, "original"));
        peer_lock(&first, "original", "SHARE UPDATE EXCLUSIVE", false);
        peer_lock(&first, "child", "ACCESS EXCLUSIVE", true);
        assert_eq!(
            sql(&second, "SELECT count(*) AS n FROM child WHERE v IS NULL").rows[0]["n"],
            Value::Int(1)
        );
        sql(&second, "ROLLBACK");
    }
}
