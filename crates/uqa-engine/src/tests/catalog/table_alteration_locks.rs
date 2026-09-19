//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! ALTER actions retain the required relation locks on each object they change or reference.

use crate::tests::relation_lock_support::{after_wait, error, sessions, sql};
use crate::Engine;
use uqa_core::Value;

mod catalog_addresses;
mod constraint_rename;
mod foreign_key_removal;
mod foreign_key_rename;
mod removal;
mod validation;

fn peer_lock(engine: &Engine, table: &str, mode: &str, allowed: bool) {
    sql(engine, "BEGIN");
    let result = engine.sql(
        &format!("LOCK TABLE ONLY {table} IN {mode} MODE NOWAIT"),
        &[],
    );
    sql(engine, "ROLLBACK");
    if allowed {
        result.unwrap_or_else(|error| panic!("{table}/{mode}: {error}"));
    } else {
        let error = result.unwrap_err();
        assert_eq!(error.sqlstate(), Some("55P03"), "{table}/{mode}: {error}");
    }
}

#[test]
fn trigger_mode_changes_allow_readers_and_exclude_writers() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "BEGIN; ALTER TABLE t DISABLE TRIGGER USER");
        peer_lock(&second, "t", "ACCESS SHARE", true);
        peer_lock(&second, "t", "ROW EXCLUSIVE", false);
        sql(&first, "ROLLBACK");
    }
}

#[test]
fn check_validation_allows_writers_and_excludes_other_validation() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(
            &first,
            "ALTER TABLE t ADD CONSTRAINT positive CHECK(v>0) NOT VALID",
        );
        sql(&first, "BEGIN; ALTER TABLE t VALIDATE CONSTRAINT positive");
        peer_lock(&second, "t", "ROW EXCLUSIVE", true);
        peer_lock(&second, "t", "SHARE UPDATE EXCLUSIVE", false);
        sql(&first, "ROLLBACK");
    }
}

#[test]
fn foreign_key_addition_retains_a_reference_lock() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE p(id integer PRIMARY KEY)");
        sql(
            &first,
            "BEGIN; ALTER TABLE t ADD CONSTRAINT fk FOREIGN KEY(v) REFERENCES p(id) NOT VALID",
        );
        peer_lock(&second, "p", "ROW EXCLUSIVE", false);
        peer_lock(&second, "p", "ACCESS SHARE", true);
        peer_lock(&second, "t", "ACCESS SHARE", true);
        sql(&first, "ROLLBACK");
    }
}

#[test]
fn partition_attachment_keeps_parent_writes_available() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE p(v integer) PARTITION BY RANGE(v)");
        sql(
            &first,
            "BEGIN; ALTER TABLE p ATTACH PARTITION t FOR VALUES FROM(0) TO(10)",
        );
        peer_lock(&second, "p", "ROW EXCLUSIVE", true);
        peer_lock(&second, "t", "ACCESS SHARE", false);
        sql(&first, "ROLLBACK");
    }
}

#[test]
fn inherited_column_addition_locks_descendants() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE child() INHERITS(t)");
        sql(&first, "BEGIN; ALTER TABLE t ADD COLUMN extra integer");
        peer_lock(&second, "child", "ACCESS SHARE", false);
        peer_lock(&second, "t", "ACCESS SHARE", false);
        sql(&first, "ROLLBACK");
    }
}

#[test]
fn inherited_check_validation_preserves_child_writes() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE child() INHERITS(t); ALTER TABLE t ADD CONSTRAINT positive CHECK(v>0) NOT VALID");
        sql(&first, "BEGIN; ALTER TABLE t VALIDATE CONSTRAINT positive");
        peer_lock(&second, "child", "ROW EXCLUSIVE", true);
        peer_lock(&second, "child", "SHARE UPDATE EXCLUSIVE", false);
        sql(&first, "ROLLBACK");
    }
}

#[test]
fn combined_alter_actions_retain_the_strongest_lock() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(
            &first,
            "BEGIN; ALTER TABLE t DISABLE TRIGGER USER, ADD COLUMN extra integer",
        );
        peer_lock(&second, "t", "ACCESS SHARE", false);
        sql(&first, "ROLLBACK");
    }
}

#[test]
fn inheritance_parent_modes_and_ownership_follow_the_requested_action() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE p(v integer)");
        sql(&first, "BEGIN; ALTER TABLE t INHERIT p");
        peer_lock(&second, "p", "ROW EXCLUSIVE", true);
        peer_lock(&second, "p", "SHARE UPDATE EXCLUSIVE", false);
        sql(&first, "COMMIT");
        sql(&first, "CREATE ROLE reader; GRANT CREATE ON SCHEMA public TO reader; ALTER TABLE t OWNER TO reader; SET ROLE reader");
        sql(&first, "BEGIN; ALTER TABLE t NO INHERIT p");
        peer_lock(&second, "p", "ROW EXCLUSIVE", true);
        peer_lock(&second, "p", "ACCESS EXCLUSIVE", false);
        sql(&first, "COMMIT");
        error(&first, "ALTER TABLE t INHERIT p", "42501");
    }
}

#[test]
fn foreign_key_binding_follows_the_referenced_name_after_replacement() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE p(id integer PRIMARY KEY)");
        sql(
            &first,
            "BEGIN; DROP TABLE p; CREATE TABLE p(id integer PRIMARY KEY)",
        );
        let (second, result) = after_wait(
            &first,
            second,
            "ALTER TABLE t ADD CONSTRAINT fk FOREIGN KEY(v) REFERENCES p(id) NOT VALID",
            "public.p",
            "COMMIT",
        );
        result.unwrap();
        sql(&first, "INSERT INTO p VALUES(2)");
        sql(&second, "INSERT INTO t VALUES(2)");
        error(&second, "INSERT INTO t VALUES(3)", "23503");
    }
}

#[test]
fn foreign_key_binding_rechecks_references_privilege_after_waiting() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE p(id integer PRIMARY KEY); CREATE ROLE reader; GRANT CREATE ON SCHEMA public TO reader; ALTER TABLE t OWNER TO reader; GRANT REFERENCES ON p TO reader");
        sql(&second, "SET ROLE reader");
        sql(
            &first,
            "BEGIN; ALTER TABLE p ADD COLUMN extra integer; REVOKE REFERENCES ON p FROM reader",
        );
        let (_, result) = after_wait(
            &first,
            second,
            "ALTER TABLE t ADD CONSTRAINT fk FOREIGN KEY(v) REFERENCES p(id) NOT VALID",
            "public.p",
            "COMMIT",
        );
        let denied = result.unwrap_err();
        assert_eq!(denied.sqlstate(), Some("42501"), "{provider}: {denied}");
    }
}

#[test]
fn inherited_alteration_locks_follow_child_identity_through_rename() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE child() INHERITS(t)");
        sql(
            &first,
            "BEGIN; ALTER TABLE child RENAME TO renamed; CREATE TABLE child(v integer)",
        );
        let (second, result) = after_wait(
            &first,
            second,
            "ALTER TABLE t ADD COLUMN extra integer",
            "public.child",
            "COMMIT",
        );
        result.unwrap();
        assert_eq!(sql(&second, "SELECT count(*) AS n FROM information_schema.columns WHERE table_schema='public' AND table_name='renamed' AND column_name='extra'").rows[0]["n"], Value::Int(1));
        assert_eq!(sql(&second, "SELECT count(*) AS n FROM information_schema.columns WHERE table_schema='public' AND table_name='child' AND column_name='extra'").rows[0]["n"], Value::Int(0));
    }
}

#[test]
fn savepoint_rollback_releases_primary_and_referenced_definition_locks() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE p(id integer PRIMARY KEY)");
        sql(&first, "BEGIN; SAVEPOINT before_fk; ALTER TABLE t ADD CONSTRAINT fk FOREIGN KEY(v) REFERENCES p(id) NOT VALID; ROLLBACK TO before_fk");
        peer_lock(&second, "t", "ACCESS EXCLUSIVE", true);
        peer_lock(&second, "p", "ACCESS EXCLUSIVE", true);
        sql(&first, "ROLLBACK");
    }
}

#[test]
fn skipped_column_addition_does_not_bind_a_foreign_key_reference() {
    let engine = Engine::new();
    sql(&engine, "CREATE TABLE t(v integer)");
    sql(
        &engine,
        "ALTER TABLE t ADD COLUMN IF NOT EXISTS v integer REFERENCES missing(id)",
    );
    assert_eq!(engine.take_sql_notices().len(), 1);
}

#[test]
fn later_alter_action_waits_preserve_prior_private_column_changes() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE p(id integer PRIMARY KEY)");
        sql(&first, "BEGIN; ALTER TABLE p ADD COLUMN changed integer");
        let (second, result) = after_wait(&first, second,
            "ALTER TABLE t ADD COLUMN added integer DEFAULT 7, ADD CONSTRAINT fk FOREIGN KEY(v) REFERENCES p(id) NOT VALID",
            "public.p", "COMMIT");
        result.unwrap();
        assert_eq!(
            sql(&second, "SELECT added FROM t").rows[0]["added"],
            Value::Int(7)
        );
        sql(&first, "INSERT INTO p VALUES(2, NULL)");
        sql(&second, "INSERT INTO t(v) VALUES(2)");
        error(&second, "INSERT INTO t(v) VALUES(3)", "23503");
    }
}

#[test]
fn generated_check_merging_retains_prepared_descendant_locks() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE child(v integer CONSTRAINT t_v_check CHECK(v>0)) INHERITS(t); CREATE TABLE grandchild() INHERITS(child)");
        sql(&first, "BEGIN; ALTER TABLE t ADD CHECK(v>0)");
        peer_lock(&second, "child", "ACCESS SHARE", false);
        peer_lock(&second, "grandchild", "ACCESS SHARE", false);
        sql(&first, "ROLLBACK");
    }
}

#[test]
fn added_column_check_locks_descendants_beyond_a_merged_column() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE child(extra integer) INHERITS(t); CREATE TABLE grandchild() INHERITS(child)");
        sql(
            &first,
            "BEGIN; ALTER TABLE t ADD COLUMN extra integer CHECK(extra>0)",
        );
        peer_lock(&second, "grandchild", "ACCESS SHARE", false);
        sql(&first, "ROLLBACK");
    }
}

#[test]
fn noninherited_constraint_additions_still_lock_inheritors_during_preparation() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE p(id integer PRIMARY KEY); CREATE TABLE child() INHERITS(t); CREATE TABLE grandchild() INHERITS(child)");
        sql(&first, "BEGIN; ALTER TABLE t ADD CHECK(v>0) NO INHERIT");
        peer_lock(&second, "grandchild", "ACCESS SHARE", false);
        sql(&first, "ROLLBACK");
        sql(
            &first,
            "BEGIN; ALTER TABLE t ADD CONSTRAINT fk FOREIGN KEY(v) REFERENCES p(id) NOT VALID",
        );
        peer_lock(&second, "grandchild", "ACCESS SHARE", true);
        peer_lock(&second, "grandchild", "ROW EXCLUSIVE", false);
        sql(&first, "ROLLBACK");
    }
}

#[test]
fn check_validation_locks_descendants_beyond_an_already_validated_child() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE child() INHERITS(t); CREATE TABLE grandchild() INHERITS(child); ALTER TABLE t ADD CONSTRAINT positive CHECK(v>0) NOT VALID; ALTER TABLE child VALIDATE CONSTRAINT positive");
        sql(&first, "BEGIN; ALTER TABLE t VALIDATE CONSTRAINT positive");
        peer_lock(&second, "grandchild", "ROW EXCLUSIVE", true);
        peer_lock(&second, "grandchild", "SHARE UPDATE EXCLUSIVE", false);
        sql(&first, "ROLLBACK");
    }
}

#[test]
fn attachment_locks_new_default_descendants_published_while_waiting() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE p(v integer) PARTITION BY RANGE(v); CREATE TABLE fallback PARTITION OF p DEFAULT PARTITION BY RANGE(v); CREATE TABLE leaf(v integer)");
        sql(
            &first,
            "BEGIN; ALTER TABLE fallback ATTACH PARTITION leaf FOR VALUES FROM(20) TO(30)",
        );
        let (second, result) = after_wait(
            &first,
            second,
            "BEGIN; ALTER TABLE p ATTACH PARTITION t FOR VALUES FROM(0) TO(10)",
            "public.fallback",
            "COMMIT",
        );
        result.unwrap();
        peer_lock(&first, "leaf", "ACCESS SHARE", false);
        sql(&second, "ROLLBACK");
    }
}
