//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{rows, Json, Provider};

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn live_io_timing_settings_and_restores_match_postgresql(#[case] provider: Provider) {
    let reference: Json = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/io_timing.expected.json"
    ))
    .unwrap();
    assert!(reference["provenance"]["version"]
        .as_str()
        .unwrap()
        .starts_with("PostgreSQL 18."));
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("io-settings.db"));
    for (index, case) in reference["settings"].as_array().unwrap().iter().enumerate() {
        let result = engine
            .sql(case["sql"].as_str().unwrap(), &[])
            .and_then(|result| rows(&engine, &result));
        match result {
            Ok(actual) => {
                assert_eq!(case["sqlstate"], "", "{index}: {case}");
                assert_eq!(actual, case["rows"], "{index}: {case}");
            }
            Err(error) => {
                assert_eq!(
                    error.sqlstate().unwrap_or_default(),
                    case["sqlstate"].as_str().unwrap(),
                    "{index}: {case}: {error}"
                );
                assert_eq!(
                    error.to_string(),
                    case["message"].as_str().unwrap(),
                    "{index}: {case}"
                );
            }
        }
        let transaction = if engine.transaction_failed() {
            3
        } else if engine.transaction_depth() != 0 {
            2
        } else {
            0
        };
        assert_eq!(transaction, case["transaction_status"], "{index}: {case}");
        if let Some(expected) = case["effective_timing"].as_bool() {
            assert_eq!(
                engine.runtime.diagnostics.io_timing(),
                expected,
                "{index}: {case}"
            );
        }
    }
    engine.close().unwrap();
    let engine = provider.open(&directory.path().join("io-startup.db"));
    engine
        .set_client_parameter("track_io_timing", "on")
        .unwrap();
    assert!(engine.runtime.diagnostics.io_timing());
    engine.sql("BEGIN; SET track_io_timing = off; SAVEPOINT disabled; SET LOCAL track_io_timing = on; ROLLBACK TO disabled", &[]).unwrap();
    assert!(!engine.runtime.diagnostics.io_timing());
    engine.rollback().unwrap();
    assert!(engine.runtime.diagnostics.io_timing());
    engine
        .sql("SET track_io_timing = off; RESET ALL", &[])
        .unwrap();
    assert!(engine.runtime.diagnostics.io_timing());
    engine.sql("DISCARD ALL", &[]).unwrap();
    assert!(engine.runtime.diagnostics.io_timing());
    let unrelated = crate::Engine::new();
    assert!(!unrelated.runtime.diagnostics.io_timing());
    engine.close().unwrap();
}
