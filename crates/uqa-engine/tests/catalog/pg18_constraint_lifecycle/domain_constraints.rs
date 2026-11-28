//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Domain constraint changes preserve catalog transactions and enforce new values on every provider.

use std::{path::Path, sync::Arc};
use tempfile::TempDir;
use uqa_engine::Engine;

const ORACLE: &str =
    include_str!("../../../../../tests/parity/pg18/alter_domain_constraints_oracle.expected.json");

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn domain_defaults_match_postgresql_and_persistent_reopen(#[case] provider: usize) {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("domain-defaults.db");
    let oracle =
        include_str!("../../../../../tests/parity/pg18/alter_domain_defaults_oracle.expected.json");
    let engine = open(provider, &path);
    crate::pg18_oracle::verify(&engine, oracle);
    if provider != 0 {
        drop(engine);
        let reopened = open(provider, &path);
        let oracle: serde_json::Value = serde_json::from_str(oracle).unwrap();
        let expected = oracle["cases"].as_array().unwrap().last().unwrap();
        let actual = crate::pg18_oracle::run_case(&reopened, expected["sql"].as_str().unwrap());
        assert_eq!(actual["error"], expected["error"]);
        assert_eq!(actual["results"], expected["results"]);
    }
}

#[rstest::rstest]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn an_unrelated_commit_preserves_the_prepared_default_of_a_private_table(#[case] provider: usize) {
    let directory = TempDir::new().unwrap();
    let engine = open(provider, &directory.path().join("private-domain.db"));
    let peer = engine.new_session().unwrap();
    engine
        .sql("CREATE TABLE unrelated(id integer)", &[])
        .unwrap();
    engine
        .sql(
            "BEGIN;
             CREATE FUNCTION bound_default() RETURNS integer LANGUAGE sql IMMUTABLE AS 'SELECT 14';
             CREATE DOMAIN written_number AS integer DEFAULT bound_default();
             CREATE TABLE writes(id integer, v written_number);
             PREPARE put(integer) AS INSERT INTO writes(id) VALUES($1) RETURNING v::integer AS v",
            &[],
        )
        .unwrap();
    peer.sql("INSERT INTO unrelated VALUES(1)", &[]).unwrap();
    engine
        .sql("ALTER DOMAIN written_number SET DEFAULT 25", &[])
        .unwrap();
    let first = engine.sql("EXECUTE put(1)", &[]).unwrap();
    assert_eq!(first.rows[0]["v"], uqa_core::Value::Int(14));
    engine
        .sql(
            "CREATE OR REPLACE FUNCTION bound_default() RETURNS integer
             LANGUAGE sql IMMUTABLE AS 'SELECT 37'",
            &[],
        )
        .unwrap();
    let second = engine.sql("EXECUTE put(2)", &[]).unwrap();
    assert_eq!(second.rows[0]["v"], uqa_core::Value::Int(25));
    engine.sql("ROLLBACK", &[]).unwrap();
    assert_eq!(
        peer.sql("SELECT id FROM unrelated", &[]).unwrap().rows[0]["id"],
        uqa_core::Value::Int(1)
    );
}

#[test]
fn host_prepared_write_keeps_only_the_defaults_it_uses() {
    use uqa_core::Value;
    use uqa_sql::SQLParam;

    let engine = Engine::new();
    for sql in [
        "CREATE FUNCTION unused_default() RETURNS integer LANGUAGE sql AS 'SELECT 7'",
        "CREATE DOMAIN written_value AS integer DEFAULT unused_default()",
        "CREATE DOMAIN omitted_value AS integer DEFAULT 12",
        "CREATE TABLE host_defaults(a written_value, b omitted_value)",
    ] {
        engine.sql(sql, &[]).unwrap();
    }
    let write = engine
        .prepare_statement(
            "INSERT INTO host_defaults(a) VALUES($1) RETURNING b::integer AS value",
            &[],
        )
        .unwrap();
    engine
        .sql("ALTER DOMAIN omitted_value SET DEFAULT 13", &[])
        .unwrap();
    engine.sql("CREATE OR REPLACE FUNCTION unused_default() RETURNS integer LANGUAGE sql AS 'SELECT 8'", &[]).unwrap();
    let execute = || {
        engine
            .execute_prepared_statement(&write, &[SQLParam::scalar(Value::Int(42))])
            .unwrap()
            .rows[0]["value"]
            .clone()
    };
    assert_eq!(execute(), Value::Int(12));
    engine
        .sql("ALTER TABLE host_defaults ADD COLUMN extra text", &[])
        .unwrap();
    assert_eq!(execute(), Value::Int(13));
    engine.finalize_prepared_statement(&write);
}

fn open(provider: usize, path: &Path) -> Engine {
    match provider {
        0 => Engine::new(),
        1 => Engine::open(path).unwrap(),
        2 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap(),
        ))
        .unwrap(),
        3 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_redb::RedbStorage::open(path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn domain_constraints_match_postgresql(#[case] provider: usize) {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("domain-constraints.db");
    let engine = open(provider, &path);
    crate::pg18_oracle::verify(&engine, ORACLE);
    if provider != 0 {
        drop(engine);
        let reopened = open(provider, &path);
        let oracle: serde_json::Value = serde_json::from_str(ORACLE).unwrap();
        let expected = oracle["cases"].as_array().unwrap().last().unwrap();
        let actual = crate::pg18_oracle::run_case(&reopened, expected["sql"].as_str().unwrap());
        assert_eq!(actual["error"], expected["error"]);
        assert_eq!(actual["results"], expected["results"]);
    }
}

#[rstest::rstest]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn domain_validation_reads_latest_rows_without_changing_the_query_snapshot(
    #[case] provider: usize,
) {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("domain-snapshots.db");
    let engine = open(provider, &path);
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/domain_constraint_snapshots.expected.json"
    ))
    .unwrap();
    for schedule in oracle["schedules"].as_array().unwrap() {
        let peer = engine.new_session().unwrap();
        for step in schedule["steps"].as_array().unwrap() {
            let session = if step["session"] == "A" {
                &engine
            } else {
                &peer
            };
            let sql = step["sql"].as_str().unwrap();
            let actual = crate::pg18_oracle::run_case(session, sql);
            if let Some(state) = step["result"]["sqlstate"].as_str() {
                assert_eq!(actual["error"]["sqlstate"], state, "{sql}: {actual}");
                assert_eq!(
                    actual["error"]["message"], step["result"]["message"],
                    "{sql}"
                );
            } else {
                assert!(actual["error"].is_null(), "{sql}: {actual}");
                assert_eq!(
                    actual["results"][0]["rows"], step["result"]["rows"],
                    "{sql}"
                );
            }
        }
    }
    let peer = engine.new_session().unwrap();
    for sql in [
        "CREATE DOMAIN latest_good AS integer",
        "CREATE TABLE latest_good_rows(value latest_good)",
        "BEGIN ISOLATION LEVEL REPEATABLE READ",
        "SELECT count(*) FROM latest_good_rows",
    ] {
        engine.sql(sql, &[]).unwrap();
    }
    peer.sql("INSERT INTO latest_good_rows VALUES(1)", &[])
        .unwrap();
    engine
        .sql("ALTER DOMAIN latest_good ADD CHECK(VALUE>0)", &[])
        .unwrap();
    assert_eq!(
        engine
            .sql("SELECT count(*) AS n FROM latest_good_rows", &[])
            .unwrap()
            .rows[0]["n"],
        uqa_core::Value::Int(0)
    );
    engine.sql("ROLLBACK", &[]).unwrap();
}
