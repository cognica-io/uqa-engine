//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Atomic routine removal against concurrent registration and stale preflight inputs.

use std::sync::{mpsc, Arc};

use uqa_sql::{
    ast::{CreateFunction, DropFunctionStmt, Statement},
    SQLError,
};

use super::{canonical_routine_type_name, routine_signature_types};
use crate::Engine;

fn create_function(sql: &str) -> CreateFunction {
    let mut statements = uqa_sql::compile(sql).expect("compile CREATE FUNCTION");
    assert_eq!(statements.len(), 1);
    let Statement::CreateFunction(definition) = statements.remove(0) else {
        panic!("expected CREATE FUNCTION statement");
    };
    *definition
}

fn drop_function(sql: &str) -> DropFunctionStmt {
    let mut statements = uqa_sql::compile(sql).expect("compile DROP FUNCTION");
    assert_eq!(statements.len(), 1);
    let Statement::DropFunction(statement) = statements.remove(0) else {
        panic!("expected DROP FUNCTION statement");
    };
    statement
}

fn has_function(engine: &Engine, name: &str, argument_types: &[&str]) -> bool {
    let expected = argument_types
        .iter()
        .map(|type_name| canonical_routine_type_name(type_name))
        .collect::<Vec<_>>();
    engine
        .durable
        .sql_user_functions
        .read()
        .get(name)
        .is_some_and(|overloads| {
            overloads
                .iter()
                .any(|function| routine_signature_types(&function.def) == expected)
        })
}

#[test]
fn drop_preserves_registration_completed_after_dependency_preflight() {
    let engine = Arc::new(Engine::new());
    engine
        .register_sql_function(create_function(
            "CREATE FUNCTION public.drop_target() RETURNS INTEGER LANGUAGE SQL IMMUTABLE AS 'SELECT 1'",
        ))
        .unwrap();
    let drop_statement = drop_function("DROP FUNCTION public.drop_target()");
    let (preflight_complete_tx, preflight_complete_rx) = mpsc::sync_channel(0);
    let (continue_tx, continue_rx) = mpsc::sync_channel(0);
    let drop_engine = Arc::clone(&engine);
    let drop_thread = std::thread::spawn(move || {
        let plan = drop_engine
            .preflight_sql_function_drop(&drop_statement)
            .unwrap();
        preflight_complete_tx.send(()).unwrap();
        continue_rx.recv().unwrap();
        drop_engine.commit_sql_function_drop(plan)
    });

    preflight_complete_rx.recv().unwrap();
    engine
        .register_sql_function(create_function(
            "CREATE FUNCTION public.drop_target(value INTEGER) RETURNS INTEGER LANGUAGE SQL IMMUTABLE AS 'SELECT $1'",
        ))
        .unwrap();
    continue_tx.send(()).unwrap();
    drop_thread.join().unwrap().unwrap();

    assert!(!has_function(&engine, "public.drop_target", &[]));
    assert!(has_function(&engine, "public.drop_target", &["INTEGER"]));
}

#[test]
fn multi_target_drop_revalidation_is_atomic() {
    let engine = Engine::new();
    for sql in [
        "CREATE FUNCTION public.drop_first() RETURNS INTEGER LANGUAGE SQL IMMUTABLE AS 'SELECT 1'",
        "CREATE FUNCTION public.drop_second() RETURNS INTEGER LANGUAGE SQL IMMUTABLE AS 'SELECT 2'",
    ] {
        engine.register_sql_function(create_function(sql)).unwrap();
    }
    let plan = engine
        .preflight_sql_function_drop(&drop_function(
            "DROP FUNCTION public.drop_first(), public.drop_second()",
        ))
        .unwrap();
    engine
        .drop_sql_functions(&drop_function("DROP FUNCTION public.drop_second()"))
        .unwrap();

    let error = engine.commit_sql_function_drop(plan).unwrap_err();
    assert!(matches!(error, SQLError::Internal(_)), "{error}");
    assert!(has_function(&engine, "public.drop_first", &[]));
    assert!(!has_function(&engine, "public.drop_second", &[]));
}
