//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{tests::providers::Provider, Engine};

// The reference includes compiler scratch beyond the default retention allowance. Compare its unchanged complexity diagnostic with explicit headroom; the constrained test below still verifies rejection and recovery.
const ORACLE_ALLOWANCE: usize = 256 << 20;

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite(Provider::SQLiteNative)]
#[case::sqlite_key_value(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn regular_expression_engine_matches_postgresql_and_reopen(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("regular-expression-engine.db");
    let engine = provider.open_with_retention(&path, ORACLE_ALLOWANCE);
    let mut reference: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/regular_expression_engine_oracle.expected.json"
    )))
    .unwrap();
    crate::tests::pg18_oracle::verify(&engine, &reference.to_string());
    if matches!(provider, Provider::Memory) {
        return;
    }
    drop(engine);
    // A stored view and CHECK constraints keep the engine's matching and their input collations after reopening, and the row a stored default produced reads back unchanged.
    let engine = provider.open_with_retention(&path, ORACLE_ALLOWANCE);
    reference["cases"].as_array_mut().unwrap().retain(|case| {
        matches!(
            case["id"].as_str().unwrap(),
            "stored_view_rows"
                | "stored_check_rejects_c_class"
                | "stored_check_rejects_case"
                | "stored_check_rows"
                | "stored_default_values"
        )
    });
    assert_eq!(reference["cases"].as_array().unwrap().len(), 5);
    crate::tests::pg18_oracle::verify(&engine, &reference.to_string());
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite(Provider::SQLiteNative)]
#[case::sqlite_key_value(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn regex_compilation_preserves_native_admission_and_recovers(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open_with_retention(&directory.path().join("regex-limit.db"), 64 << 20);
    let retained = engine.query_retention_control().unwrap();
    assert_eq!(retained.memory().used(), 0);
    let error = engine
        .sql(
            "SELECT regexp_match('ab', '((((a{1,100}){1,100}){1,100}){1,100})')",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"), "{provider:?}: {error}");
    // Failed compilation releases its scratch; parsed/logical SQL still belongs
    // to the session cache. Retain that exact owner across recovery and teardown.
    let cached = engine.session.state.read().sql_statement_cache.clone();
    let cached_bytes = retained.memory().used();
    assert!(cached_bytes > 0);
    assert_eq!(engine.transaction_depth(), 0);
    assert_eq!(engine.sql("SELECT 1 AS value", &[]).unwrap().rows.len(), 1);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(retained.memory().used(), cached_bytes);
    drop(cached);
    assert_eq!(retained.memory().used(), 0);
}

#[test]
fn a_long_match_stops_at_the_statement_timeout() {
    // Dissecting 3000 nested capture groups takes seconds in PostgreSQL 18.4 and here. The engine's interrupt checks end it at the statement timeout, as CHECK_FOR_INTERRUPTS does in PostgreSQL, in a scalar call and in a table function alike.
    let engine = Engine::new();
    engine.sql("SET statement_timeout = 50", &[]).unwrap();
    for query in [
        "SELECT regexp_match('a', repeat('(', 3000) || 'a' || repeat(')', 3000)) AS captures",
        "SELECT * FROM regexp_matches('a', repeat('(', 3000) || 'a' || repeat(')', 3000)) AS m(captures)",
    ] {
        let started = std::time::Instant::now();
        let error = engine.sql(query, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"), "{query}: {error}");
        assert_eq!(
            error.to_string(),
            "canceling statement due to statement timeout"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "{query} ran to completion instead of stopping at the timeout"
        );
    }
    assert_eq!(engine.sql("SELECT 1 AS value", &[]).unwrap().rows.len(), 1);
}
