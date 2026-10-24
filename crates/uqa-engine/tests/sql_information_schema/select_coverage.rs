//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native scope and persistent-provider acceptance for complete SELECT coverage.

use uqa_engine::{sql::format_postgres_text, Engine};

fn verify(engine: &Engine, reference: &serde_json::Value) {
    engine
        .sql(
            &format!(
                "SET search_path = {}",
                reference["search_path"].as_str().unwrap()
            ),
            &[],
        )
        .unwrap();
    engine
        .sql(
            &format!("SET ROLE {}", reference["role"].as_str().unwrap()),
            &[],
        )
        .unwrap();
    for case in reference["cases"].as_array().unwrap() {
        let sql = case["sql"].as_str().unwrap();
        let result = engine.sql(sql, &[]);
        if let Some(expected) = case.get("error") {
            let error = result.unwrap_err();
            assert_eq!(
                error.sqlstate(),
                expected["sqlstate"].as_str(),
                "{sql}: {error}"
            );
            assert_eq!(
                error.to_string(),
                expected["message"].as_str().unwrap(),
                "{sql}"
            );
            continue;
        }
        let result = result.unwrap_or_else(|error| panic!("{sql}: {error}"));
        let expected = case["rows"].as_array().unwrap();
        assert_eq!(result.rows.len(), expected.len(), "{sql}");
        for (row, expected) in expected.iter().enumerate() {
            let fields = expected.as_object().unwrap();
            assert_eq!(result.columns.len(), fields.len(), "{sql}");
            for (column, name) in result.columns.iter().enumerate() {
                let expected = &fields[name];
                let text = expected
                    .as_str()
                    .map_or_else(|| expected.to_string(), str::to_owned);
                assert_eq!(
                    format_postgres_text(
                        result.value_at(row, column).unwrap(),
                        result.column_types[column].as_ref().unwrap(),
                        Some(engine)
                    )
                    .unwrap(),
                    text,
                    "{sql}: {row}/{name}"
                );
            }
        }
    }
    engine.sql("RESET ROLE", &[]).unwrap();
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn derived_select_authority_matches_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("select-coverage.db");
    let open = || match provider {
        0 => Engine::new(),
        1 => Engine::open(&path).unwrap(),
        2 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(&path).unwrap(),
        ))
        .unwrap(),
        3 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_redb::RedbStorage::open(&path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    };
    let reference: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../uqa-sql/src/semantics/privileges/coverage/tests/pg18_select_coverage.json"
    )))
    .unwrap();
    let engine = open();
    for sql in reference["setup"].as_array().unwrap() {
        engine.sql(sql.as_str().unwrap(), &[]).unwrap();
    }
    verify(&engine, &reference);
    if provider != 0 {
        drop(engine);
        verify(&open(), &reference);
    }
}
