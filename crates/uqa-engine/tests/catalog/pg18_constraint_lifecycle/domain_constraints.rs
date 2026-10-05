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
