//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Session date-order inputs retain `PostgreSQL` values, diagnostics and stored constants.

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
fn date_order_matches_postgresql_and_survives_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("datestyle.db");
    let engine = open(provider, &path);
    let reference =
        include_str!("../../../../tests/parity/pg18/datestyle_input_oracle.expected.json");
    crate::pg18_oracle::verify(&engine, reference);
    if provider == 0 {
        return;
    }
    drop(engine);
    let engine = open(provider, &path);
    let mut reopened: serde_json::Value = serde_json::from_str(reference).unwrap();
    reopened["cases"].as_array_mut().unwrap().retain(|case| {
        matches!(
            case["id"].as_str(),
            Some("reopen_setting" | "reopen_values")
        )
    });
    assert_eq!(reopened["cases"].as_array().unwrap().len(), 2);
    crate::pg18_oracle::verify(&engine, &reopened.to_string());
}

#[test]
fn direct_preparation_and_streaming_read_their_session_date_order() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("sessions.db")).unwrap();
    let other = engine.new_session().unwrap();
    engine.set_variable("DateStyle", "ISO, DMY").unwrap();
    engine
        .register_prepared(
            "direct_date".into(),
            uqa_sql::compile("SELECT '02/03/2020'::date AS d")
                .unwrap()
                .remove(0),
        )
        .unwrap();
    engine.set_variable("DateStyle", "ISO, MDY").unwrap();
    assert_eq!(super::text(&engine, "EXECUTE direct_date"), "2020-03-02");
    assert_eq!(
        super::text(&other, "SELECT '02/03/2020'::date"),
        "2020-02-03"
    );
    for (setting, expected) in [("ISO, DMY", "2020-03-02"), ("ISO, MDY", "2020-02-03")] {
        engine.set_variable("DateStyle", setting).unwrap();
        let cursor = engine
            .sql_cursor("SELECT '02/03/2020'::date AS d", &[])
            .unwrap();
        let rows = cursor
            .flat_map(|batch| batch.unwrap().into_rows())
            .collect::<Vec<_>>();
        let uqa_core::Value::Temporal(date) = &rows[0]["d"] else {
            panic!("date")
        };
        assert_eq!(date.to_sql_string(), expected);
    }
}
