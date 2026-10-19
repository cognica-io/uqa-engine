//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent type-introspection values and catalog metadata through the public SQL API.

use std::{path::Path, sync::Arc};
use uqa_engine::Engine;

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
fn type_introspection_values_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("type-introspection.db"));
    crate::pg18_oracle::verify(
        &engine,
        include_str!(
            "../../../uqa-execution/src/catalog/projection/builtin_routines/tests/type_introspection.json"
        ),
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn type_observation_arguments_match_postgresql_after_reopen(#[case] provider: usize) {
    for (fixture, first_probe, schema) in [
        (
            include_str!(
                "../../../../tests/parity/pg18/type_observation_arguments_oracle.expected.json"
            ),
            "nulls_savepoint",
            "typeof_argument_probe",
        ),
        (
            include_str!(
                "../../../../tests/parity/pg18/type_observation_security_oracle.expected.json"
            ),
            "view_input_savepoint",
            "type_observer_security_probe",
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("type-observation.db");
        let engine = open(provider, &path);
        let mut reference: serde_json::Value = serde_json::from_str(fixture).unwrap();
        let cases = reference["cases"].take().as_array().unwrap().clone();
        let split = cases
            .iter()
            .position(|case| case["id"] == first_probe)
            .unwrap();
        reference["cases"] = serde_json::Value::from(cases[..split].to_vec());
        crate::pg18_oracle::verify(&engine, &reference.to_string());
        engine.sql("COMMIT", &[]).unwrap();
        let engine = if provider == 0 {
            engine
        } else {
            drop(engine);
            open(provider, &path)
        };
        engine
            .sql(
                &format!("BEGIN; SET LOCAL search_path={schema},pg_catalog"),
                &[],
            )
            .unwrap();
        reference["cases"] = serde_json::Value::from(cases[split..].to_vec());
        crate::pg18_oracle::verify(&engine, &reference.to_string());
    }
}
