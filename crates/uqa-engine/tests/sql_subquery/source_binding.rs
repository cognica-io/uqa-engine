//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_engine::{sql::format_postgres_text, Engine};

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn derived_source_bindings_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("derived-source-bindings.db");
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
        "/../uqa-sql/src/binding/execution/tests/pg18_derived_sources.json"
    )))
    .unwrap();
    let verify = |engine: &Engine| {
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
            let columns = case["columns"].as_array().unwrap();
            assert_eq!(result.columns.len(), columns.len(), "{sql}");
            for (position, expected) in columns.iter().enumerate() {
                assert_eq!(
                    result.columns[position],
                    expected[0].as_str().unwrap(),
                    "{sql}"
                );
                assert_eq!(
                    result.column_types[position]
                        .as_ref()
                        .unwrap()
                        .regtype_name(),
                    expected[1].as_str().unwrap(),
                    "{sql}"
                );
            }
            let rows = (0..result.rows.len())
                .map(|row| {
                    (0..result.columns.len())
                        .map(|column| {
                            format_postgres_text(
                                result.value_at(row, column).unwrap(),
                                result.column_types[column].as_ref().unwrap(),
                                Some(engine),
                            )
                            .unwrap()
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            assert_eq!(serde_json::json!(rows), case["rows"], "{sql}");
        }
    };
    let engine = open();
    for sql in reference["setup"].as_array().unwrap() {
        engine.sql(sql.as_str().unwrap(), &[]).unwrap();
    }
    verify(&engine);
    engine.sql("PREPARE source_parameter(integer) AS SELECT id, label FROM (SELECT id, label FROM source_input WHERE id = $1 OFFSET 0) AS derived", &[]).unwrap();
    for value in [1, 2, 1] {
        let result = engine
            .sql(&format!("EXECUTE source_parameter({value})"), &[])
            .unwrap();
        assert_eq!(result.value_at(0, 0), Some(&uqa_core::Value::Int(value)));
        assert_eq!(result.rows.len(), 1);
    }
    engine.sql("DEALLOCATE source_parameter", &[]).unwrap();
    if provider != 0 {
        drop(engine);
        verify(&open());
    }
}
