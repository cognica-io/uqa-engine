//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! ALTER resolves ownership before relation kind and retains namespace authority through waits.

use crate::tests::relation_lock_support::{after_wait, sessions, sql};
use crate::Engine;

fn rejection(engine: &Engine, statement: &str, state: &str, message: &str) {
    let error = engine.sql(statement, &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some(state), "{statement}: {error}");
    assert!(error.to_string().contains(message), "{statement}: {error}");
}

#[test]
fn sequence_alteration_checks_actual_relation_ownership_before_kind() {
    for provider in 0..3 {
        let (_directory, engine, _) = sessions(provider);
        sql(&engine, "CREATE ROLE reader; CREATE SCHEMA s; GRANT USAGE ON SCHEMA s TO reader; CREATE TABLE s.wrong(id integer); SET ROLE reader");
        for action in [
            "INCREMENT BY 2",
            "RENAME TO renamed",
            "SET SCHEMA missing",
            "SET LOGGED",
        ] {
            rejection(
                &engine,
                &format!("ALTER SEQUENCE s.wrong {action}"),
                "42501",
                "must be owner of table wrong",
            );
        }
        sql(
            &engine,
            "RESET ROLE; ALTER TABLE s.wrong OWNER TO reader; SET ROLE reader",
        );
        rejection(
            &engine,
            "ALTER SEQUENCE s.wrong INCREMENT BY 2",
            "42809",
            "cannot open relation \"wrong\"",
        );
        rejection(
            &engine,
            "ALTER SEQUENCE s.wrong SET SCHEMA missing",
            "42809",
            "\"wrong\" is not a sequence",
        );
        rejection(
            &engine,
            "ALTER SEQUENCE s.wrong RENAME TO renamed",
            "42501",
            "permission denied for schema s",
        );
        sql(
            &engine,
            "RESET ROLE; GRANT CREATE ON SCHEMA s TO reader; SET ROLE reader",
        );
        rejection(
            &engine,
            "ALTER SEQUENCE s.wrong RENAME TO renamed",
            "42809",
            "\"wrong\" is not a sequence",
        );
    }
}

#[test]
fn sequence_rename_requires_source_create_while_definition_and_move_do_not() {
    for provider in 0..3 {
        let (_directory, engine, _) = sessions(provider);
        sql(&engine, "CREATE ROLE reader; CREATE SCHEMA source; CREATE SCHEMA target; GRANT USAGE, CREATE ON SCHEMA source, target TO reader; CREATE SEQUENCE source.ids; ALTER SEQUENCE source.ids OWNER TO reader; REVOKE CREATE ON SCHEMA source FROM reader; SET ROLE reader");
        rejection(
            &engine,
            "ALTER SEQUENCE source.ids RENAME TO renamed",
            "42501",
            "permission denied for schema source",
        );
        sql(
            &engine,
            "ALTER SEQUENCE source.ids INCREMENT BY 2; ALTER SEQUENCE source.ids SET SCHEMA target",
        );
        assert!(engine.sequence_state("target.ids").unwrap().is_some());
    }
}

#[test]
fn sequence_rename_rechecks_source_create_after_relation_waits() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE ROLE reader; CREATE SCHEMA s; GRANT USAGE, CREATE ON SCHEMA s TO reader; CREATE SEQUENCE s.ids; ALTER SEQUENCE s.ids OWNER TO reader");
            sql(
                &second,
                &format!("SET ROLE reader; BEGIN ISOLATION LEVEL {isolation}; SELECT 1"),
            );
            sql(
                &first,
                "BEGIN; ALTER SEQUENCE s.ids INCREMENT BY 2; REVOKE CREATE ON SCHEMA s FROM reader",
            );
            let (second, result) = after_wait(
                &first,
                second,
                "ALTER SEQUENCE s.ids RENAME TO renamed",
                "s.ids",
                "COMMIT",
            );
            let error = result.unwrap_err();
            assert_eq!(
                error.sqlstate(),
                Some("42501"),
                "{provider}/{isolation}: {error}"
            );
            assert!(error.to_string().contains("permission denied for schema s"));
            sql(&second, "ROLLBACK");
            assert!(second.sequence_state("s.ids").unwrap().is_some());
        }
    }
}

#[test]
fn temporary_sequence_rename_requires_current_database_temp_authority() {
    let engine = Engine::new();
    sql(&engine, "CREATE ROLE reader; REVOKE TEMP ON DATABASE uqa FROM PUBLIC; GRANT TEMP ON DATABASE uqa TO reader; SET ROLE reader; CREATE TEMP SEQUENCE ids; RESET ROLE; REVOKE TEMP ON DATABASE uqa FROM reader; SET ROLE reader");
    rejection(
        &engine,
        "ALTER SEQUENCE pg_temp.ids RENAME TO renamed",
        "42501",
        "permission denied for schema pg_temp_",
    );
    sql(&engine, "ALTER SEQUENCE pg_temp.ids INCREMENT BY 2");
}

#[test]
fn sequence_alteration_checks_view_foreign_and_index_owners_before_kind() {
    let engine = Engine::new();
    sql(&engine, "CREATE ROLE reader; CREATE SCHEMA s; GRANT USAGE, CREATE ON SCHEMA s TO reader; CREATE TABLE s.base(id integer); CREATE INDEX wrong_index ON s.base(id); CREATE TABLE s.keyed(id integer PRIMARY KEY); CREATE VIEW s.wrong_view AS SELECT 1 AS id; CREATE MATERIALIZED VIEW s.wrong_materialized AS SELECT 1 AS id; CREATE SERVER remote FOREIGN DATA WRAPPER memory_fdw; CREATE FOREIGN TABLE s.wrong_foreign(id integer) SERVER remote; SET ROLE reader");
    for (name, kind) in [
        ("wrong_view", "view"),
        ("wrong_materialized", "materialized view"),
        ("wrong_foreign", "foreign table"),
        ("wrong_index", "index"),
        ("keyed_pkey", "index"),
    ] {
        rejection(
            &engine,
            &format!("ALTER SEQUENCE s.{name} INCREMENT BY 2"),
            "42501",
            &format!("must be owner of {kind} {name}"),
        );
    }
    sql(&engine, "RESET ROLE; ALTER TABLE s.base OWNER TO reader; ALTER TABLE s.keyed OWNER TO reader; ALTER VIEW s.wrong_view OWNER TO reader; ALTER MATERIALIZED VIEW s.wrong_materialized OWNER TO reader; ALTER FOREIGN TABLE s.wrong_foreign OWNER TO reader; SET ROLE reader");
    for name in [
        "wrong_view",
        "wrong_materialized",
        "wrong_foreign",
        "wrong_index",
        "keyed_pkey",
    ] {
        rejection(
            &engine,
            &format!("ALTER SEQUENCE s.{name} INCREMENT BY 2"),
            "42809",
            &format!("cannot open relation \"{name}\""),
        );
    }
}

#[test]
fn sequence_alteration_checks_catalog_ownership_and_pinned_protection_before_kind() {
    let engine = Engine::new();
    sql(&engine, "CREATE ROLE reader; SET ROLE reader");
    rejection(
        &engine,
        "ALTER SEQUENCE pg_catalog.pg_class INCREMENT BY 2",
        "42501",
        "must be owner of table pg_class",
    );
    sql(&engine, "RESET ROLE");
    rejection(
        &engine,
        "ALTER SEQUENCE pg_catalog.pg_class INCREMENT BY 2",
        "42501",
        "is a system catalog",
    );
    rejection(
        &engine,
        "ALTER SEQUENCE pg_catalog.pg_shadow INCREMENT BY 2",
        "42809",
        "cannot open relation \"pg_shadow\"",
    );
}
