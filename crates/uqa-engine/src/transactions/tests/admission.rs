//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::Engine;
use std::cell::Cell;
use uqa_execution::statement::batch::context::BatchTransactions;
use uqa_sql::SQLError;

#[rstest::rstest]
#[case::memory(false)]
#[case::native_sqlite(true)]
fn admitted_operations_own_failure_and_unwind_recovery(#[case] persistent: bool) {
    let directory = tempfile::tempdir().unwrap();
    let engine = if persistent {
        Engine::open(&directory.path().join("admission.db")).unwrap()
    } else {
        Engine::new()
    };
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

#[test]
fn sql_and_prepared_callback_panics_restore_the_owned_savepoint() {
    use std::sync::Arc;
    use uqa_core::Value;

    let engine = Arc::new(Engine::new());
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
