//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{tests::providers::Provider, SQLError};

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn operation_facts_follow_confirmed_boundaries_and_preserve_rows(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("completion.db"));
    let outcome = engine.execute_with_completion(|engine| {
        engine.sql("CREATE TABLE items (n integer PRIMARY KEY)", &[])
    });
    outcome.result.unwrap();
    assert_eq!(outcome.completion, ExecutionCompletion::Committed);
    engine.sql("INSERT INTO items VALUES (1)", &[]).unwrap();

    // A previous commit must not be attributed to this parse failure.
    let outcome = engine.execute_with_completion(|engine| engine.sql("SELECT )", &[]));
    assert_eq!(outcome.result.unwrap_err().sqlstate(), Some("42601"));
    assert_eq!(outcome.completion, ExecutionCompletion::None);

    let outcome =
        engine.execute_with_completion(|engine| engine.sql("INSERT INTO items VALUES (1)", &[]));
    assert_eq!(outcome.result.unwrap_err().sqlstate(), Some("23505"));
    assert_eq!(outcome.completion, ExecutionCompletion::RolledBack);
    assert_eq!(engine.transaction_depth(), 0);

    engine
        .sql("BEGIN; INSERT INTO items VALUES (2)", &[])
        .unwrap();
    let outcome =
        engine.execute_with_completion(|engine| engine.sql("INSERT INTO items VALUES (1)", &[]));
    assert_eq!(outcome.result.unwrap_err().sqlstate(), Some("23505"));
    assert_eq!(outcome.completion, ExecutionCompletion::InTransaction);
    assert!(engine.transaction_failed());
    let outcome = engine.execute_with_completion(Engine::rollback);
    outcome.result.unwrap();
    assert_eq!(outcome.completion, ExecutionCompletion::RolledBack);
    assert_eq!(
        engine.sql("SELECT * FROM items", &[]).unwrap().rows.len(),
        1
    );

    // The first segment commits and the second rolls back. A single rollback
    // label would incorrectly imply that the committed insertion disappeared.
    let outcome = engine.execute_with_completion(|engine| {
        engine.sql(
            "INSERT INTO items VALUES (2); COMMIT; INSERT INTO items VALUES (2)",
            &[],
        )
    });
    assert_eq!(outcome.result.unwrap_err().sqlstate(), Some("23505"));
    assert_eq!(outcome.completion, ExecutionCompletion::Unknown);
    assert_eq!(
        engine.sql("SELECT * FROM items", &[]).unwrap().rows.len(),
        2
    );

    let outcome = engine.execute_with_completion(|engine| {
        engine.sql_batch_diagnosed(&[
            ("INSERT INTO items VALUES (3)", &[]),
            ("INSERT INTO items VALUES (1)", &[]),
        ])
    });
    let error = outcome.result.unwrap_err();
    assert_eq!(error.statement_index, Some(1));
    assert_eq!(error.error.sqlstate(), Some("23505"));
    assert_eq!(outcome.completion, ExecutionCompletion::RolledBack);
    assert_eq!(
        engine.sql("SELECT * FROM items", &[]).unwrap().rows.len(),
        2
    );

    // A fallible host publication after the native commit retains that fact.
    let outcome = engine.execute_with_completion(|engine| {
        engine.sql("INSERT INTO items VALUES (3)", &[])?;
        Err::<(), _>(SQLError::Internal("host publication failed".into()))
    });
    assert_eq!(outcome.completion, ExecutionCompletion::Committed);
    assert!(outcome
        .result
        .unwrap_err()
        .to_string()
        .contains("host publication failed"));
    assert_eq!(
        engine.sql("SELECT * FROM items", &[]).unwrap().rows.len(),
        3
    );

    engine
        .sql(
            "CREATE TABLE deferred_items (n integer REFERENCES items(n) DEFERRABLE INITIALLY DEFERRED)",
            &[],
        )
        .unwrap();
    engine
        .sql("BEGIN; INSERT INTO deferred_items VALUES (999)", &[])
        .unwrap();
    let outcome = engine.execute_with_completion(Engine::commit);
    assert_eq!(outcome.result.unwrap_err().sqlstate(), Some("23503"));
    assert_eq!(outcome.completion, ExecutionCompletion::RolledBack);
    assert_eq!(engine.transaction_depth(), 0);
    assert!(engine
        .sql("SELECT * FROM deferred_items", &[])
        .unwrap()
        .rows
        .is_empty());
}

#[test]
fn pending_resolution_preserves_its_original_identity() {
    use uqa_storage::mvcc::{DatabaseId, StorageTransactionId, TransactionOutcomeId};
    let engine = Engine::new();
    engine.begin().unwrap();
    let identity = TransactionOutcomeId::Records(
        StorageTransactionId::new(DatabaseId::from_bytes([42; 16]), 9).unwrap(),
    );
    for status in [
        TransactionStatus::CommitPending(identity),
        TransactionStatus::RollbackPending(identity),
    ] {
        engine.session.transactions.lock()[0].status = status;
        let outcome = engine.execute_with_completion(|engine| engine.sql("SELECT 1", &[]));
        assert_eq!(outcome.result.unwrap_err().sqlstate(), Some("08007"));
        assert_eq!(outcome.completion, ExecutionCompletion::Pending);
        assert_eq!(engine.pending_transaction_completion(), Some(identity));
    }
    engine.session.transactions.lock()[0].status = TransactionStatus::Active;
    engine.rollback().unwrap();
}

#[test]
fn saturated_or_uncertain_history_cannot_report_a_false_completion() {
    let before = CompletionCounters::default();
    for after in [
        CompletionCounters {
            committed: u64::MAX,
            ..before
        },
        CompletionCounters {
            rolled_back: u64::MAX,
            ..before
        },
        CompletionCounters {
            unknown: 1,
            ..before
        },
    ] {
        for success in [false, true] {
            assert_eq!(
                after.completion_since(before, success),
                ExecutionCompletion::Unknown
            );
        }
    }
}
