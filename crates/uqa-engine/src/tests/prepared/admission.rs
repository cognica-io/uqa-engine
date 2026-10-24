//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::Engine;
use serde_json::Value as Json;

#[rstest::rstest]
#[case::memory(false)]
#[case::native_sqlite(true)]
fn prepared_admission_matches_postgresql(#[case] persistent: bool) {
    let reference: Json = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/prepared_admission_oracle.expected.json"
    ))
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let engine = if persistent {
        Engine::open(&directory.path().join("admission.db")).unwrap()
    } else {
        Engine::new()
    };
    super::reference::verify(&engine, &reference);
}

#[rstest::rstest]
#[case::memory(false)]
#[case::native_sqlite(true)]
fn empty_declaration_errors_and_native_registration_share_recovery(#[case] persistent: bool) {
    let directory = tempfile::tempdir().unwrap();
    let engine = if persistent {
        Engine::open(&directory.path().join("declaration.db")).unwrap()
    } else {
        Engine::new()
    };
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
