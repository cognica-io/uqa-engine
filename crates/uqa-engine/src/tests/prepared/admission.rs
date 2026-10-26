//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::tests::providers::Provider;
use serde_json::Value as Json;

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn prepared_admission_matches_postgresql(#[case] provider: Provider) {
    let reference: Json = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/prepared_admission_oracle.expected.json"
    ))
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("admission.db"));
    super::reference::verify(&engine, &reference);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn empty_declaration_errors_and_native_registration_share_recovery(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("declaration.db"));
    engine.sql("BEGIN; SAVEPOINT retry", &[]).unwrap();
    let error = engine
        .prepare_statement(
            "",
            &[uqa_sql::ColumnType::Named("missing_prepared_type".into())],
        )
        .err()
        .unwrap();
    assert_eq!(error.sqlstate(), Some("42704"));
    assert!(engine.transaction_failed());
    engine
        .register_prepared(
            "native_recovery".into(),
            uqa_sql::compile("ROLLBACK TO retry").unwrap().remove(0),
        )
        .unwrap();
    assert!(engine.lookup_prepared("native_recovery").is_some());
    assert!(engine.transaction_failed());
    let recovery = engine.prepare_statement("ROLLBACK TO retry", &[]).unwrap();
    engine.execute_prepared_statement(&recovery, &[]).unwrap();
    assert!(!engine.transaction_failed());
    engine.finalize_prepared_statement(&recovery);
    engine.sql("ROLLBACK; DEALLOCATE ALL", &[]).unwrap();
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn cte_preparation_uses_the_native_allowance_and_recovers_after_rejection(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("cte-admission.db"));
    let queries = [
        "WITH q AS MATERIALIZED (VALUES ($1::integer)) SELECT * FROM q",
        "WITH q AS NOT MATERIALIZED (VALUES ($1::integer)), r AS (SELECT * FROM q) SELECT * FROM r",
        "WITH q AS (VALUES ($1::integer)) SELECT * FROM (WITH q AS (SELECT * FROM q) SELECT * FROM q) nested",
    ];
    let params = [uqa_sql::SQLParam::Scalar(uqa_core::Value::Int(7))];
    let retained = engine.query_retention_control().unwrap();
    let memory = retained.memory();
    for query in queries {
        let prepared = engine.prepare_statement(query, &[]).unwrap();
        for mode in ["force_custom_plan", "force_generic_plan"] {
            engine
                .sql(&format!("SET plan_cache_mode = {mode}"), &[])
                .unwrap();
            for explicit in [false, true] {
                if explicit {
                    engine.sql("BEGIN; SAVEPOINT admission_retry", &[]).unwrap();
                }
                let full = memory.reserve(memory.limit() - memory.used()).unwrap();
                let error = engine
                    .execute_prepared_statement(&prepared, &params)
                    .unwrap_err();
                assert_eq!(
                    error.sqlstate(),
                    Some("53200"),
                    "{mode}, explicit={explicit}: {error}"
                );
                drop(full);
                if explicit {
                    assert!(engine.transaction_failed());
                    engine.sql("ROLLBACK TO admission_retry", &[]).unwrap();
                } else {
                    assert_eq!(engine.transaction_depth(), 0);
                }
                let result = engine
                    .execute_prepared_statement(&prepared, &params)
                    .unwrap();
                assert_eq!(result.value_at(0, 0), Some(&uqa_core::Value::Int(7)));
                drop(result);
                if explicit {
                    engine.sql("ROLLBACK", &[]).unwrap();
                }
            }
        }
        engine.finalize_prepared_statement(&prepared);
    }
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}
