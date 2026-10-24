//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public catalog-selected casts retain identity through sessions and durable reopening.

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
fn catalog_cast_qualification_matches_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("catalog-casts.db");
    let engine = open(provider, &path);
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/catalog_cast_qualification_oracle.expected.json"
    ))
    .unwrap();
    let mut initial = reference.clone();
    initial["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| case["reopen"] != true);
    crate::pg18_oracle::verify(&engine, &initial.to_string());
    let engine = if provider == 0 {
        engine
    } else {
        drop(engine);
        open(provider, &path)
    };
    let mut durable = reference;
    durable["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| case["reopen"] == true);
    crate::pg18_oracle::verify(&engine, &durable.to_string());
}

#[test]
fn prepared_temporal_casts_keep_live_settings_and_rollback() {
    temporal_casts_keep_live_settings("date");
}

#[test]
fn catalog_temporal_casts_keep_live_settings_and_rollback() {
    temporal_casts_keep_live_settings("public.scoped_date");
}

fn temporal_casts_keep_live_settings(target: &str) {
    use uqa_core::Value;
    let engine = Engine::new();
    engine
        .sql(
            "CREATE DOMAIN public.scoped_date AS date CHECK (VALUE > DATE '2000-01-01');
             CREATE TABLE cast_rows(id integer, v text);
             INSERT INTO cast_rows VALUES (1,'01/02/2020'), (2,'01/02/2020');
             SET DateStyle = 'ISO, MDY'",
            &[],
        )
        .unwrap();
    let result = engine
        .sql(
            &format!("SELECT id, set_config('DateStyle', CASE id WHEN 1 THEN 'ISO, MDY' ELSE 'ISO, DMY' END, false) AS setting,
                    v::{target} - DATE '1970-01-01' AS day FROM cast_rows"),
            &[],
        )
        .unwrap();
    let days = result
        .rows
        .iter()
        .map(|row| (row["id"].clone(), row["day"].clone()))
        .collect::<Vec<_>>();
    assert_eq!(
        days,
        [
            (Value::Int(1), Value::Int(18_263)),
            (Value::Int(2), Value::Int(18_293)),
        ],
    );
    engine
        .sql(
            &format!("SET DateStyle = 'ISO, MDY';
             PREPARE cast_dates AS SELECT v::{target} - DATE '1970-01-01' AS day FROM cast_rows ORDER BY id"),
            &[],
        )
        .unwrap();
    for (setup, expected) in [
        ("SELECT 1", 18_263),
        ("BEGIN; SET LOCAL DateStyle = 'ISO, DMY'", 18_293),
        ("ROLLBACK", 18_263),
    ] {
        engine.sql(setup, &[]).unwrap();
        let result = engine.sql("EXECUTE cast_dates", &[]).unwrap();
        assert_eq!(result.rows.len(), 2);
        assert!(result
            .rows
            .iter()
            .all(|row| row["day"] == Value::Int(expected)));
    }
}
