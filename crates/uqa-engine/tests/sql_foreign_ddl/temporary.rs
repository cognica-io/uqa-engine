//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::Value;
use uqa_engine::Engine;

fn open(provider: usize, path: &std::path::Path) -> Engine {
    match provider {
        0 => Engine::new(),
        1 => Engine::open(path).unwrap(),
        2 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap(),
        ))
        .unwrap(),
        3 => Engine::from_persistent_provider(std::sync::Arc::new(
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
fn temporary_foreign_lifecycle_matches_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("temporary-foreign.db");
    let engine = open(provider, &path);
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/temporary_foreign_tables_oracle.expected.json"
    ))
    .unwrap();
    let mut initial = reference.clone();
    initial["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| case["reopen"] != true);
    assert_eq!(initial["cases"].as_array().unwrap().len(), 38);
    crate::pg18_oracle::verify(&engine, &initial.to_string());
    engine
        .load_memory_foreign_table(
            "pg_temp.retained_foreign",
            vec![[("n".into(), Value::Int(7))].into()],
        )
        .unwrap();
    assert_eq!(
        engine
            .sql("SELECT n FROM pg_temp.retained_foreign", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(7)
    );
    engine.sql("DISCARD TEMP; CREATE FOREIGN TABLE pg_temp.retained_foreign(n integer) SERVER temporary_foreign_server", &[]).unwrap();
    let unloaded = engine
        .sql("SELECT n FROM pg_temp.retained_foreign", &[])
        .unwrap_err();
    assert!(
        unloaded.to_string().contains("no loaded memory data"),
        "{unloaded}"
    );
    engine
        .load_memory_foreign_table(
            "pg_temp.retained_foreign",
            vec![[("n".into(), Value::Int(7))].into()],
        )
        .unwrap();
    if provider == 0 {
        return;
    }
    engine.sql("CREATE ROLE copied_foreign_reader; GRANT SELECT(n) ON pg_temp.retained_foreign TO copied_foreign_reader", &[]).unwrap();
    let peer = engine.new_session().unwrap();
    engine
        .sql(
            "REVOKE SELECT(n) ON pg_temp.retained_foreign FROM copied_foreign_reader",
            &[],
        )
        .unwrap();
    peer.sql("DROP ROLE copied_foreign_reader", &[]).unwrap();
    assert_eq!(
        peer.sql("SELECT n FROM pg_temp.retained_foreign", &[])
            .unwrap_err()
            .sqlstate(),
        Some("42P01")
    );
    engine
        .sql(
            "BEGIN ISOLATION LEVEL REPEATABLE READ; SELECT n FROM pg_temp.retained_foreign",
            &[],
        )
        .unwrap();
    peer.sql("CREATE FOREIGN TABLE pg_temp.retained_foreign(n integer) SERVER temporary_foreign_server; CREATE TABLE public.peer_catalog_refresh(n integer)", &[]).unwrap();
    assert_eq!(
        engine
            .sql("SELECT n FROM pg_temp.retained_foreign", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(7)
    );
    engine.sql("COMMIT", &[]).unwrap();
    peer.load_memory_foreign_table(
        "pg_temp.retained_foreign",
        vec![[("n".into(), Value::Int(8))].into()],
    )
    .unwrap();
    assert_eq!(
        engine
            .sql("SELECT n FROM pg_temp.retained_foreign", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(7)
    );
    assert_eq!(
        peer.sql("SELECT n FROM pg_temp.retained_foreign", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(8)
    );
    drop(engine);
    assert_eq!(
        peer.sql("SELECT n FROM pg_temp.retained_foreign", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(8)
    );
    drop(peer);
    let reopened = open(provider, &path);
    let mut reference = reference;
    reference["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| case["reopen"] == true);
    assert_eq!(reference["cases"].as_array().unwrap().len(), 2);
    crate::pg18_oracle::verify(&reopened, &reference.to_string());
}

#[rstest::rstest]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn temporary_foreign_role_leases_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let source = open(provider, &directory.path().join("foreign-role-leases.db"));
    let peer = source.new_session().unwrap();
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/temporary_foreign_role_dependencies_oracle.expected.json"
    ))
    .unwrap();
    let cases = reference["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 17);
    for case in cases {
        let engine = if case["session"] == "source" {
            &source
        } else {
            &peer
        };
        let sql = case["sql"].as_str().unwrap();
        let actual = crate::pg18_oracle::run_case(engine, sql);
        assert_eq!(
            actual["error"]["sqlstate"], case["error"]["sqlstate"],
            "{sql}: {actual}"
        );
        assert_eq!(
            actual["error"]["message"], case["error"]["message"],
            "{sql}: {actual}"
        );
        let tags = actual["command_tags"].as_array().unwrap();
        assert_eq!(
            tags.last()
                .and_then(serde_json::Value::as_str)
                .unwrap_or(""),
            case["command_tag"].as_str().unwrap(),
            "{sql}"
        );
    }
}
