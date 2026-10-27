//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{SQLFunctionContext, SQLRowSource, SQLTableFunctionStream};
use std::sync::atomic::{AtomicUsize, Ordering};

struct ScanFactory(Arc<AtomicUsize>);
struct Scan(Arc<AtomicUsize>);

impl SQLTableFunction for ScanFactory {
    fn call_stream(&self, _: &[Value]) -> Result<SQLTableFunctionStream, SQLError> {
        Ok(SQLTableFunctionStream::from_source(
            ["v"],
            Scan(Arc::clone(&self.0)),
        ))
    }
}

impl SQLRowSource<Vec<Value>> for Scan {
    fn next_row(&mut self) -> Result<Option<Vec<Value>>, SQLError> {
        Ok(Some(vec![Value::Int(7)]))
    }

    fn close(&mut self) -> Result<(), SQLError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(SQLError::Routine {
            sqlstate: "58000".into(),
            message: "scan cleanup".into(),
        })
    }
}

#[test]
fn escaped_scan_holds_its_registration_until_fallible_close_completes() {
    let engine = Engine::new();
    let closed = Arc::new(AtomicUsize::new(0));
    let mut registration = engine
        .register_table_function_retained(
            "escaped",
            SQLFunctionOptions::default(),
            ScanFactory(Arc::clone(&closed)),
        )
        .unwrap();
    let function = engine
        .query_runtime_view()
        .lookup_table_function("escaped")
        .unwrap();
    let mut scan = function.function.call_stream(&[]).unwrap();
    drop(function);
    assert!(matches!(
        registration.unregister(),
        Err(SQLFunctionRegistrationError::Busy)
    ));
    assert_eq!(scan.rows.next().unwrap().unwrap(), vec![Value::Int(7)]);
    assert_eq!(scan.rows.close().unwrap_err().sqlstate(), Some("58000"));
    registration.unregister().unwrap();
    scan.rows.close().unwrap();
    assert_eq!(closed.load(Ordering::SeqCst), 1);
}

#[test]
fn internal_readers_share_context_installation_without_retaining_the_source_session() {
    for provider in 0..3 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("context.db");
        let root = match provider {
            0 => Engine::open(&path).unwrap(),
            1 => Engine::from_persistent_provider(Arc::new(
                uqa_storage_sqlite::SQLiteKeyValueStorage::open(&path).unwrap(),
            ))
            .unwrap(),
            2 => Engine::from_persistent_provider(Arc::new(
                uqa_storage_redb::RedbStorage::open(&path).unwrap(),
            ))
            .unwrap(),
            _ => unreachable!(),
        };
        let reader = root.new_internal_read_session().unwrap();
        let public = root.new_session().unwrap();
        assert!(reader
            .query_runtime_view()
            .function_context
            .get::<i64>()
            .is_none());
        root.install_sql_function_context(SQLFunctionContext::new(37_i64))
            .unwrap();
        assert_eq!(
            reader.query_runtime_view().function_context.get::<i64>(),
            Some(&37)
        );
        assert!(public
            .query_runtime_view()
            .function_context
            .get::<i64>()
            .is_none());
        let session = Arc::downgrade(&root.session);
        root.close().unwrap();
        drop(root);
        assert!(session.upgrade().is_none());
        assert_eq!(
            reader.query_runtime_view().function_context.get::<i64>(),
            Some(&37)
        );
    }
}

struct ReenterOnDrop {
    engine: std::sync::Weak<Engine>,
    dropped: Arc<AtomicUsize>,
}

impl SQLScalarFunction for ReenterOnDrop {
    fn call(&self, _: &[Value]) -> Result<Value, SQLError> {
        Ok(Value::Null)
    }
}

impl Drop for ReenterOnDrop {
    fn drop(&mut self) {
        let engine = self.engine.upgrade().unwrap();
        engine
            .register_scalar_function("after_drop", |_: &[Value]| Ok(Value::Int(1)))
            .unwrap();
        assert_eq!(
            engine.sql("SELECT after_drop() AS v", &[]).unwrap().rows[0]["v"],
            Value::Int(1)
        );
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn callback_destruction_runs_after_registry_and_session_locks_are_released() {
    let engine = Arc::new(Engine::new());
    let dropped = Arc::new(AtomicUsize::new(0));
    let mut registration = engine
        .register_scalar_function_retained(
            "retired",
            SQLFunctionOptions::default(),
            ReenterOnDrop {
                engine: Arc::downgrade(&engine),
                dropped: Arc::clone(&dropped),
            },
        )
        .unwrap();
    registration.unregister().unwrap();
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    engine
        .register_scalar_function(
            "replaced",
            ReenterOnDrop {
                engine: Arc::downgrade(&engine),
                dropped: Arc::clone(&dropped),
            },
        )
        .unwrap();
    engine
        .register_scalar_function("replaced", |_: &[Value]| Ok(Value::Null))
        .unwrap();
    assert_eq!(dropped.load(Ordering::SeqCst), 2);
}

struct CountDrop(Arc<AtomicUsize>);

impl SQLScalarFunction for CountDrop {
    fn call(&self, _: &[Value]) -> Result<Value, SQLError> {
        Ok(Value::Null)
    }
}

impl Drop for CountDrop {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn retired_handles_do_not_retain_other_callbacks_after_engine_close() {
    let engine = Engine::new();
    let dropped = Arc::new(AtomicUsize::new(0));
    let mut registration = engine
        .register_scalar_function_retained(
            "retired",
            SQLFunctionOptions::default(),
            |_: &[Value]| Ok(Value::Null),
        )
        .unwrap();
    engine
        .register_scalar_function("other", CountDrop(Arc::clone(&dropped)))
        .unwrap();
    registration.unregister().unwrap();
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    registration.unregister().unwrap();
}
