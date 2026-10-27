//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::tests::providers::Provider;
use crate::Engine;
use std::cell::Cell;
use uqa_execution::statement::batch::context::BatchTransactions;
use uqa_sql::SQLError;

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn admitted_operations_own_failure_and_unwind_recovery(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("admission.db"));
    engine.sql("CREATE TABLE items (n integer)", &[]).unwrap();
    for unwind in [false, true] {
        engine
            .sql("BEGIN; INSERT INTO items VALUES (1); SAVEPOINT keep", &[])
            .unwrap();
        let calls = Cell::new(0);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            BatchTransactions::with_active_transaction(&engine, &mut || {
                calls.set(calls.get() + 1);
                engine.sql("INSERT INTO items VALUES (2)", &[])?;
                assert!(!unwind, "admitted operation panic");
                Err(SQLError::Routine {
                    sqlstate: "22012".into(),
                    message: "admitted operation failure".into(),
                })
            })
        }));
        assert_eq!(calls.get(), 1);
        if unwind {
            assert!(result.is_err());
        } else {
            assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("22012"));
        }
        assert!(engine.transaction_failed());
        let error = BatchTransactions::with_active_transaction(&engine, &mut || {
            panic!("failed transactions must not invoke the operation")
        })
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("25P02"));
        assert_eq!(engine.transaction_depth(), 1);
        engine.sql("ROLLBACK TO keep", &[]).unwrap();
        assert!(!engine.transaction_failed());
        let rows = engine.sql("SELECT n FROM items", &[]).unwrap();
        assert_eq!(rows.rows.len(), 1);
        assert_eq!(rows.rows[0]["n"], uqa_core::Value::Int(1));
        engine.sql("ROLLBACK", &[]).unwrap();
    }
}

#[test]
fn unresolved_completion_rejects_operations_without_replacing_its_identity() {
    use crate::transactions::TransactionStatus;
    use uqa_storage::mvcc::{DatabaseId, StorageTransactionId, TransactionOutcomeId};

    let engine = Engine::new();
    engine.begin().unwrap();
    let identity = TransactionOutcomeId::Records(
        StorageTransactionId::new(DatabaseId::from_bytes([17; 16]), 42).unwrap(),
    );
    for status in [
        TransactionStatus::CommitPending(identity),
        TransactionStatus::RollbackPending(identity),
    ] {
        engine.session.transactions.lock()[0].status = status;
        let error = BatchTransactions::with_active_transaction(&engine, &mut || {
            panic!("unresolved completion must not invoke a new operation")
        })
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("08007"));
        assert_eq!(engine.pending_transaction_completion(), Some(identity));
    }
    engine.session.transactions.lock()[0].status = TransactionStatus::Active;
    engine.rollback().unwrap();
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn sql_and_prepared_callback_panics_restore_the_owned_savepoint(#[case] provider: Provider) {
    use std::sync::Arc;
    use uqa_core::Value;

    let directory = tempfile::tempdir().unwrap();
    let engine = Arc::new(provider.open(&directory.path().join("panic-admission.db")));
    engine.sql("CREATE TABLE items (n integer)", &[]).unwrap();
    let weak = Arc::downgrade(&engine);
    engine
        .register_scalar_function(
            "fail_after_write",
            move |_args: &[Value]| -> Result<Value, SQLError> {
                weak.upgrade()
                    .unwrap()
                    .sql("INSERT INTO items VALUES (2)", &[])?;
                panic!("host callback panic");
            },
        )
        .unwrap();
    let prepared = engine
        .prepare_statement("SELECT fail_after_write()", &[])
        .unwrap();
    for use_prepared in [false, true] {
        engine
            .sql("BEGIN; INSERT INTO items VALUES (1); SAVEPOINT keep", &[])
            .unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if use_prepared {
                engine.execute_prepared_statement(&prepared, &[])
            } else {
                engine.sql("SELECT fail_after_write()", &[])
            }
        }));
        assert!(result.is_err());
        assert!(engine.transaction_failed());
        engine.sql("ROLLBACK TO keep", &[]).unwrap();
        let rows = engine.sql("SELECT n FROM items", &[]).unwrap();
        assert_eq!(rows.rows.len(), 1);
        assert_eq!(rows.rows[0]["n"], Value::Int(1));
        engine.sql("ROLLBACK", &[]).unwrap();
    }
    engine.finalize_prepared_statement(&prepared);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn cleanup_resource_failure_preserves_the_original_diagnostic_and_batch_member(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("cleanup-diagnostics.db"));
    engine
        .sql(
            "CREATE TABLE items (n integer); INSERT INTO items VALUES (7)",
            &[],
        )
        .unwrap();
    let control = engine.query_retention_control().unwrap();
    let held = std::cell::RefCell::new(None);
    let fail = || {
        *held.borrow_mut() = Some(
            control
                .memory()
                .reserve(control.memory().limit() - control.memory().used())
                .unwrap(),
        );
        SQLError::Diagnostic {
            sqlstate: "22012".into(),
            message: "original operation failure".into(),
            detail: Some("original detail".into()),
            hint: Some("original hint".into()),
        }
        .at_position(Some(17))
    };
    for explicit in [false, true] {
        if explicit {
            engine.sql("BEGIN; SAVEPOINT retry", &[]).unwrap();
        }
        let error = if explicit {
            engine
                .run_existing_transaction_operation(|| Err::<(), _>(fail()), std::convert::identity)
        } else {
            engine.transaction(|_| Err::<(), _>(fail()))
        }
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("22012"));
        assert_eq!(error.to_string(), "original operation failure");
        assert_eq!(error.position(), Some(17));
        assert_eq!(error.hint(), Some("original hint"));
        assert!(error.detail().unwrap().starts_with("original detail"));
        if !matches!(provider, Provider::Memory) {
            assert!(error.cleanup().is_some(), "{error:?}");
            assert!(error.detail().unwrap().contains("memory requires"));
        }
        drop(held.borrow_mut().take());
        if explicit {
            assert!(engine.transaction_failed());
            engine.sql("ROLLBACK TO retry; ROLLBACK", &[]).unwrap();
        }
        assert_eq!(engine.transaction_depth(), 0);
        assert_eq!(
            engine
                .sql("SELECT n FROM items", &[])
                .unwrap()
                .value_at(0, 0),
            Some(&uqa_core::Value::Int(7))
        );
    }
    let error = engine
        .transaction_with_error(
            |_| Err::<(), _>(uqa_sql::SQLBatchError::statement(fail(), 5)),
            uqa_sql::SQLBatchError::transaction,
        )
        .unwrap_err();
    assert_eq!(error.statement_index, Some(5));
    assert_eq!(error.error.sqlstate(), Some("22012"));
    assert_eq!(error.error.position(), Some(17));
    drop(held.borrow_mut().take());
    assert_eq!(engine.transaction_depth(), 0);
    assert_eq!(
        engine
            .sql("SELECT n FROM items", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&uqa_core::Value::Int(7))
    );
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn sql_cleanup_keeps_the_callback_failure_across_entry_points(#[case] provider: Provider) {
    use std::sync::Arc;
    use uqa_core::Value;

    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("sql-cleanup-diagnostics.db"));
    engine
        .sql(
            "CREATE TABLE items (n integer); INSERT INTO items VALUES (7)",
            &[],
        )
        .unwrap();
    let control = engine.query_retention_control().unwrap();
    let held = Arc::new(parking_lot::Mutex::new(None));
    let callback_held = Arc::clone(&held);
    engine
        .register_scalar_function("fail_with_retained_memory", move |_args: &[Value]| {
            *callback_held.lock() = Some(
                control
                    .memory()
                    .reserve(control.memory().limit() - control.memory().used())
                    .unwrap(),
            );
            Err(SQLError::Diagnostic {
                sqlstate: "22012".into(),
                message: "original callback failure".into(),
                detail: Some("original callback detail".into()),
                hint: Some("original callback hint".into()),
            })
        })
        .unwrap();
    let prepared = engine
        .prepare_statement("SELECT fail_with_retained_memory()", &[])
        .unwrap();
    for entry in ["SQL", "Simple Query", "prepared"] {
        for explicit in [false, true] {
            if explicit {
                engine.sql("BEGIN; SAVEPOINT retry", &[]).unwrap();
            }
            let error = match entry {
                "SQL" => engine
                    .sql("SELECT fail_with_retained_memory()", &[])
                    .unwrap_err(),
                "Simple Query" => engine
                    .sql_simple_query("SELECT fail_with_retained_memory()", &[], |_| Ok(()))
                    .unwrap_err(),
                "prepared" => engine
                    .execute_prepared_statement(&prepared, &[])
                    .unwrap_err(),
                _ => unreachable!(),
            };
            assert_eq!(
                error.sqlstate(),
                Some("22012"),
                "{entry}, explicit={explicit}: {error:?}"
            );
            assert_eq!(error.hint(), Some("original callback hint"));
            assert!(error
                .detail()
                .unwrap()
                .starts_with("original callback detail"));
            if !matches!(provider, Provider::Memory) {
                assert!(
                    error.detail().unwrap().contains("memory requires"),
                    "{entry}: {error:?}"
                );
            }
            drop(held.lock().take());
            if explicit {
                assert!(engine.transaction_failed());
                engine.sql("ROLLBACK TO retry; ROLLBACK", &[]).unwrap();
            }
            assert_eq!(engine.transaction_depth(), 0);
            assert_eq!(
                engine
                    .sql("SELECT n FROM items", &[])
                    .unwrap()
                    .value_at(0, 0),
                Some(&Value::Int(7))
            );
        }
    }
    engine.finalize_prepared_statement(&prepared);
}
