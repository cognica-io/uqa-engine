//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! ALTER TABLE retains the requested source name and current authority through definition waits.

use crate::tests::relation_lock_support::{after_wait, error, sessions, sql};
use crate::Engine;
use uqa_core::Value;

fn denied_rename(engine: &Engine, statement: &str, schema: &str) {
    let error = engine.sql(statement, &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42501"), "{statement}: {error}");
    assert!(
        error
            .to_string()
            .contains(&format!("permission denied for schema {schema}")),
        "{error}"
    );
}

#[test]
fn table_rename_requires_source_create_without_restricting_column_changes() {
    for provider in 0..3 {
        let (_directory, engine, _) = sessions(provider);
        sql(&engine, "CREATE ROLE reader; CREATE SCHEMA s; GRANT USAGE, CREATE ON SCHEMA s TO reader; CREATE TABLE s.items(id integer); ALTER TABLE s.items OWNER TO reader; REVOKE CREATE ON SCHEMA s FROM reader; SET ROLE reader");
        denied_rename(&engine, "ALTER TABLE s.items RENAME TO renamed", "s");
        sql(&engine, "ALTER TABLE s.items ADD COLUMN extra integer");
    }
}

#[test]
fn temporary_table_rename_requires_current_database_temp_authority() {
    let engine = Engine::new();
    sql(&engine, "CREATE ROLE reader; REVOKE TEMP ON DATABASE uqa FROM PUBLIC; GRANT TEMP ON DATABASE uqa TO reader; SET ROLE reader; CREATE TEMP TABLE items(id integer); RESET ROLE; REVOKE TEMP ON DATABASE uqa FROM reader; SET ROLE reader");
    denied_rename(
        &engine,
        "ALTER TABLE pg_temp.items RENAME TO renamed",
        "pg_temp_",
    );
    sql(
        &engine,
        "ALTER TABLE pg_temp.items ADD COLUMN extra integer",
    );
}

#[test]
fn table_alter_if_exists_rechecks_a_source_removed_while_waiting() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "BEGIN; ALTER TABLE t RENAME TO renamed");
        let (second, result) = after_wait(
            &first,
            second,
            "ALTER TABLE IF EXISTS t ADD COLUMN extra integer",
            "public.t",
            "COMMIT",
        );
        result.unwrap();
        assert_eq!(
            second.take_sql_notices(),
            [(
                "NOTICE".into(),
                "relation \"t\" does not exist, skipping".into()
            )]
        );
        let columns = sql(&second, "SELECT column_name FROM information_schema.columns WHERE table_schema='public' AND table_name='renamed' ORDER BY ordinal_position");
        assert_eq!(columns.rows.len(), 1);
        assert_eq!(columns.rows[0]["column_name"], Value::Str("v".into()));
    }
}

#[test]
fn table_alteration_dispatches_the_relation_kind_selected_after_waiting() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE ROLE reader; GRANT CREATE ON SCHEMA public TO reader; BEGIN; DROP TABLE t; CREATE VIEW t AS SELECT 2 AS v");
        let (second, result) = after_wait(
            &first,
            second,
            "ALTER TABLE t OWNER TO reader",
            "public.t",
            "COMMIT",
        );
        result.unwrap();
        assert_eq!(
            sql(
                &second,
                "SELECT viewowner FROM pg_views WHERE schemaname='public' AND viewname='t'"
            )
            .rows[0]["viewowner"],
            Value::Str("reader".into())
        );
    }
}

#[test]
fn table_rename_rechecks_source_create_after_definition_waits() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE ROLE reader; CREATE SCHEMA s; GRANT USAGE, CREATE ON SCHEMA s TO reader; CREATE TABLE s.items(id integer); ALTER TABLE s.items OWNER TO reader");
            sql(&second, &format!("SET ROLE reader; BEGIN ISOLATION LEVEL {isolation}; SELECT 1; SAVEPOINT before_rename"));
            sql(&first, "BEGIN; ALTER TABLE s.items ADD COLUMN extra integer; REVOKE CREATE ON SCHEMA s FROM reader");
            let (second, result) = after_wait(
                &first,
                second,
                "ALTER TABLE s.items RENAME TO renamed",
                "s.items",
                "COMMIT",
            );
            let error = result.unwrap_err();
            assert_eq!(
                error.sqlstate(),
                Some("42501"),
                "{provider}/{isolation}: {error}"
            );
            assert!(error.to_string().contains("permission denied for schema s"));
            sql(&second, "ROLLBACK TO before_rename");
            let probe = first.sql(
                "BEGIN; LOCK TABLE s.items IN ACCESS EXCLUSIVE MODE NOWAIT",
                &[],
            );
            sql(&first, "ROLLBACK");
            probe.unwrap_or_else(|error| panic!("{provider}/{isolation}: {error}; retained locks: {:?}", sql(&first, "SELECT pid, mode, granted, relation FROM pg_locks WHERE relation='s.items'::regclass")));
            sql(&second, "ROLLBACK");
        }
    }
}

#[test]
fn table_alteration_rechecks_replacement_ownership_before_kind() {
    for provider in 0..3 {
        for owned in [false, true] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE ROLE reader; GRANT CREATE ON SCHEMA public TO reader; ALTER TABLE t OWNER TO reader");
            sql(&second, "SET ROLE reader");
            sql(
                &first,
                "BEGIN; DROP TABLE t; CREATE VIEW t AS SELECT 2 AS v",
            );
            if owned {
                sql(&first, "ALTER VIEW t OWNER TO reader");
            }
            let (_, result) = after_wait(
                &first,
                second,
                "ALTER TABLE t ADD COLUMN extra integer",
                "public.t",
                "COMMIT",
            );
            let error = result.unwrap_err();
            assert_eq!(
                error.sqlstate(),
                Some(if owned { "42809" } else { "42501" }),
                "{provider}/{owned}: {error}"
            );
            if !owned {
                assert!(
                    error.to_string().contains("must be owner of view t"),
                    "{error}"
                );
            }
            assert_eq!(sql(&first, "SELECT v FROM t").rows[0]["v"], Value::Int(2));
        }
    }
}

#[test]
fn table_alteration_rebinds_an_unqualified_name_through_the_search_path() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE SCHEMA front; CREATE TABLE front.t(v integer); CREATE ROLE reader; GRANT CREATE ON SCHEMA public TO reader");
        sql(&second, "SET search_path = front, public");
        sql(&first, "BEGIN; DROP TABLE front.t");
        let (second, result) = after_wait(
            &first,
            second,
            "ALTER TABLE t OWNER TO reader",
            "front.t",
            "COMMIT",
        );
        result.unwrap();
        assert_eq!(
            sql(
                &second,
                "SELECT tableowner FROM pg_tables WHERE schemaname='public' AND tablename='t'"
            )
            .rows[0]["tableowner"],
            Value::Str("reader".into())
        );
    }
}

#[test]
fn table_alteration_reports_missing_objects_and_protects_system_relations() {
    let engine = Engine::new();
    error(
        &engine,
        "ALTER TABLE missing ADD COLUMN extra integer",
        "42P01",
    );
    error(
        &engine,
        "ALTER TABLE absent.missing ADD COLUMN extra integer",
        "3F000",
    );
    sql(&engine, "CREATE ROLE reader; SET ROLE reader");
    let denied = engine
        .sql("ALTER TABLE pg_catalog.pg_class RENAME TO renamed", &[])
        .unwrap_err();
    assert_eq!(denied.sqlstate(), Some("42501"));
    assert!(denied
        .to_string()
        .contains("must be owner of table pg_class"));
    sql(&engine, "RESET ROLE");
    let protected = engine
        .sql("ALTER TABLE pg_catalog.pg_class RENAME TO renamed", &[])
        .unwrap_err();
    assert_eq!(protected.sqlstate(), Some("42501"));
    assert!(protected.to_string().contains("is a system catalog"));
}
