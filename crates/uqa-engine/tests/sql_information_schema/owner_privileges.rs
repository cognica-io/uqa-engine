//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;

fn engine(provider: usize) -> (tempfile::TempDir, Engine) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("owner-acl.db");
    let engine = match provider {
        0 => Engine::new(),
        1 => Engine::open(&path).unwrap(),
        2 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(&path).unwrap(),
        ))
        .unwrap(),
        3 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_redb::RedbStorage::open(&path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    };
    sql(&engine, "CREATE ROLE object_owner; CREATE ROLE owner_member; GRANT object_owner TO owner_member; GRANT CREATE ON SCHEMA public TO object_owner; SET ROLE object_owner");
    (directory, engine)
}

fn sql(engine: &Engine, statement: &str) {
    engine
        .sql(statement, &[])
        .unwrap_or_else(|error| panic!("{statement}: {error}"));
}

fn scalar(engine: &Engine, statement: &str) -> Value {
    let result = engine
        .sql(statement, &[])
        .unwrap_or_else(|error| panic!("{statement}: {error}"));
    result.rows[0][&result.columns[0]].clone()
}

fn denied(engine: &Engine, statement: &str) {
    let error = engine.sql(statement, &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42501"), "{statement}: {error}");
}

#[test]
fn owner_self_revocation_controls_sequence_values_without_hiding_metadata() {
    for provider in 0..4 {
        let (_directory, engine) = engine(provider);
        sql(&engine, "CREATE SEQUENCE owned_ids CACHE 5");
        assert_eq!(
            scalar(&engine, "SELECT nextval('owned_ids')"),
            Value::Int(1)
        );
        sql(
            &engine,
            "REVOKE ALL ON SEQUENCE owned_ids FROM object_owner",
        );
        for role in ["object_owner", "owner_member"] {
            sql(&engine, &format!("SET ROLE {role}"));
            for expression in [
                "nextval('owned_ids')",
                "currval('owned_ids')",
                "lastval()",
                "setval('owned_ids', 100)",
                "pg_sequence_parameters('owned_ids'::regclass)",
            ] {
                denied(&engine, &format!("SELECT {expression}"));
            }
            denied(&engine, "SELECT * FROM owned_ids");
            assert_eq!(
                scalar(
                    &engine,
                    "SELECT pg_sequence_last_value('owned_ids'::regclass)"
                ),
                Value::Null
            );
            assert_eq!(
                scalar(
                    &engine,
                    "SELECT pg_get_sequence_data('owned_ids'::regclass)"
                ),
                Value::Record(vec![
                    ("last_value".into(), Value::Null),
                    ("is_called".into(), Value::Null)
                ])
            );
            assert_eq!(scalar(&engine, "SELECT count(*) FROM information_schema.sequences WHERE sequence_name='owned_ids'"), Value::Int(1));
            assert_eq!(
                scalar(
                    &engine,
                    "SELECT has_sequence_privilege('owned_ids', 'USAGE')"
                ),
                Value::Bool(false)
            );
            assert_eq!(
                scalar(
                    &engine,
                    "SELECT has_sequence_privilege('owned_ids', 'USAGE WITH GRANT OPTION')"
                ),
                Value::Bool(true)
            );
        }
        sql(
            &engine,
            "SET ROLE object_owner; GRANT USAGE ON SEQUENCE owned_ids TO object_owner",
        );
        assert_eq!(
            scalar(&engine, "SELECT nextval('owned_ids')"),
            Value::Int(2),
            "provider {provider} must preserve unused reservations after denial"
        );
        assert!(matches!(
            scalar(
                &engine,
                "SELECT pg_sequence_parameters('owned_ids'::regclass)"
            ),
            Value::Record(_)
        ));
        assert_eq!(
            scalar(
                &engine,
                "SELECT pg_get_sequence_data('owned_ids'::regclass)"
            ),
            Value::Record(vec![
                ("last_value".into(), Value::Null),
                ("is_called".into(), Value::Null)
            ])
        );
    }
}

#[test]
fn owner_self_revocation_controls_table_and_column_access_but_preserves_ddl_and_regrant() {
    for provider in 0..4 {
        let (_directory, engine) = engine(provider);
        sql(&engine, "CREATE TABLE owned_table(a integer, b integer); INSERT INTO owned_table VALUES (1, 2); REVOKE ALL ON TABLE owned_table FROM object_owner");
        for role in ["object_owner", "owner_member"] {
            sql(&engine, &format!("SET ROLE {role}"));
            for statement in [
                "SELECT a FROM owned_table",
                "INSERT INTO owned_table VALUES (3, 4)",
                "UPDATE owned_table SET a=3",
                "DELETE FROM owned_table",
                "TRUNCATE owned_table",
            ] {
                denied(&engine, statement);
            }
            for privilege in [
                "SELECT",
                "INSERT",
                "UPDATE",
                "DELETE",
                "TRUNCATE",
                "REFERENCES",
                "TRIGGER",
                "MAINTAIN",
            ] {
                assert_eq!(
                    scalar(
                        &engine,
                        &format!("SELECT has_table_privilege('owned_table', '{privilege}')")
                    ),
                    Value::Bool(false)
                );
                assert_eq!(scalar(&engine, &format!("SELECT has_table_privilege('owned_table', '{privilege} WITH GRANT OPTION')")), Value::Bool(true));
            }
        }
        sql(&engine, "SET ROLE object_owner; ALTER TABLE owned_table ADD COLUMN extra integer; GRANT SELECT(a) ON owned_table TO object_owner");
        for role in ["object_owner", "owner_member"] {
            sql(&engine, &format!("SET ROLE {role}"));
            assert_eq!(scalar(&engine, "SELECT a FROM owned_table"), Value::Int(1));
            denied(&engine, "SELECT b FROM owned_table");
            assert_eq!(
                scalar(
                    &engine,
                    "SELECT has_table_privilege('owned_table', 'SELECT')"
                ),
                Value::Bool(false)
            );
            assert_eq!(
                scalar(
                    &engine,
                    "SELECT has_column_privilege('owned_table', 'a', 'SELECT')"
                ),
                Value::Bool(true)
            );
            assert_eq!(
                scalar(
                    &engine,
                    "SELECT has_column_privilege('owned_table', 'b', 'SELECT')"
                ),
                Value::Bool(false)
            );
        }
        sql(&engine, "SET ROLE object_owner; GRANT SELECT ON owned_table TO object_owner; REVOKE SELECT(a) ON owned_table FROM object_owner");
        assert_eq!(
            scalar(&engine, "SELECT a+b FROM owned_table"),
            Value::Int(3)
        );
        sql(&engine, "REVOKE SELECT ON owned_table FROM object_owner");
        denied(&engine, "SELECT a FROM owned_table");
        sql(
            &engine,
            "GRANT ALL ON owned_table TO object_owner; INSERT INTO owned_table VALUES (3, 4, 5)",
        );
        assert_eq!(
            scalar(&engine, "SELECT count(*) FROM owned_table"),
            Value::Int(2)
        );
    }
}

#[test]
fn view_owners_need_explicit_access_after_revoking_their_own_acl() {
    for provider in 0..4 {
        let (_directory, engine) = engine(provider);
        sql(&engine, "CREATE TABLE view_source(a integer); INSERT INTO view_source VALUES (1); CREATE VIEW owned_view AS SELECT a FROM view_source; CREATE MATERIALIZED VIEW owned_materialized AS SELECT a FROM view_source; REVOKE ALL ON owned_view, owned_materialized FROM object_owner");
        denied(&engine, "SELECT * FROM owned_view");
        denied(&engine, "SELECT * FROM owned_materialized");
        denied(&engine, "REFRESH MATERIALIZED VIEW owned_materialized");
        sql(&engine, "GRANT SELECT(a) ON owned_view TO object_owner; GRANT MAINTAIN ON owned_materialized TO object_owner");
        assert_eq!(scalar(&engine, "SELECT a FROM owned_view"), Value::Int(1));
        sql(&engine, "REFRESH MATERIALIZED VIEW owned_materialized");
        denied(&engine, "SELECT * FROM owned_materialized");
        sql(
            &engine,
            "GRANT SELECT ON owned_materialized TO object_owner",
        );
        assert_eq!(
            scalar(&engine, "SELECT a FROM owned_materialized"),
            Value::Int(1)
        );
        sql(&engine, "REVOKE ALL ON owned_view, owned_materialized FROM object_owner; DROP VIEW owned_view; DROP MATERIALIZED VIEW owned_materialized");
    }
}
