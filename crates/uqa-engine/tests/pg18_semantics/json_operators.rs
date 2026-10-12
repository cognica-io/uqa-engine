//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

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
fn json_operators_match_postgresql_inputs_definitions_and_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("json-operators.db");
    let engine = open(provider, &path);
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("json_operators.json")).unwrap();
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
    let mut reopened = reference;
    reopened["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| case["reopen"] == true);
    crate::pg18_oracle::verify(&engine, &reopened.to_string());
    for case in initial["cases"].as_array().unwrap() {
        let Some(sql) = case["sql"]
            .as_str()
            .filter(|sql| sql.starts_with("CREATE VIEW "))
        else {
            continue;
        };
        let name = sql.split_whitespace().nth(2).unwrap();
        let expected = initial["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["sql"] == format!("SELECT * FROM {name}"))
            .unwrap();
        for pretty in [false, true] {
            let definition = engine
                .sql(
                    &format!("SELECT pg_get_viewdef('{name}'::regclass, {pretty})"),
                    &[],
                )
                .unwrap();
            let Some(uqa_core::Value::Str(definition)) = definition.value_at(0, 0) else {
                panic!("view SQL")
            };
            let recreated = format!("reconstructed_{name}_{pretty}");
            engine
                .sql(&format!("CREATE VIEW {recreated} AS {definition}"), &[])
                .unwrap();
            let mut expected = expected.clone();
            expected["sql"] = format!("SELECT * FROM {recreated}").into();
            crate::pg18_oracle::verify(
                &engine,
                &serde_json::json!({"postgresql_version": initial["postgresql_version"], "cases": [expected]}).to_string(),
            );
        }
    }
}
