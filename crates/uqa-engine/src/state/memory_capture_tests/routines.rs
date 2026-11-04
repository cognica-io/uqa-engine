//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{sql, Engine, StorageReadControl};
use std::{collections::BTreeMap, sync::Arc};
use uqa_sql::{
    catalog::definition_capture::routines::CapturedRoutines,
    routines::{CompiledFunctionBody, RoutineBody, SQLUserFunction},
};
use uqa_storage::StorageBackendError;

fn fixture(engine: &Engine) {
    sql(engine,"CREATE TABLE docs(id integer); CREATE FUNCTION bound(a integer DEFAULT 7) RETURNS integer LANGUAGE SQL STABLE SECURITY DEFINER RETURN a + 1; CREATE FUNCTION bound(a text) RETURNS text LANGUAGE SQL AS 'SELECT a'; CREATE PROCEDURE change() LANGUAGE SQL BEGIN ATOMIC INSERT INTO docs VALUES (1); END; SET check_function_bodies = off; CREATE FUNCTION deferred() RETURNS integer LANGUAGE plpgsql AS 'invalid until invoked'; RESET check_function_bodies");
}

fn same(saved: &CapturedRoutines, original: &BTreeMap<String, Vec<Arc<SQLUserFunction>>>) {
    assert_eq!(saved.len(), original.len());
    for ((name, entries), (old_name, old)) in saved.iter().zip(original) {
        assert_eq!(name, old_name);
        assert_eq!(entries.len(), old.len());
        for (value, old) in entries.iter().zip(old) {
            assert_eq!(
                serde_json::to_value(&value.definition).unwrap(),
                serde_json::to_value(&old.def).unwrap()
            );
            match (&value.bound_body, &old.body) {
                (None, RoutineBody::Source) => {}
                (Some(plans), RoutineBody::Bound(body)) => {
                    let CompiledFunctionBody::SQL(old) = body.as_ref() else {
                        panic!("SQL")
                    };
                    assert_eq!(
                        serde_json::to_value(plans).unwrap(),
                        serde_json::to_value(old).unwrap()
                    );
                }
                _ => panic!("routine body changed"),
            }
        }
    }
}

#[test]
fn memory_routine_capture_preserves_bound_and_unexamined_bodies_through_rollback_replacement_and_unread_closure(
) {
    for mutate in [false, true] {
        let engine = Engine::new();
        fixture(&engine);
        let original = engine.durable.sql_user_functions.snapshot();
        let key = engine.durable.sql_user_functions.key().unwrap();
        let control = StorageReadControl::with_limit(1 << 20);
        let cut = engine
            .storage
            .memory_capture
            .as_ref()
            .unwrap()
            .capture(&control)
            .unwrap();
        assert!(control.memory().used() < 4096);
        if mutate {
            sql(&engine,"BEGIN; CREATE OR REPLACE FUNCTION bound(a integer DEFAULT 9) RETURNS integer LANGUAGE SQL RETURN a + 2; SAVEPOINT changed; DROP PROCEDURE change(); ROLLBACK TO changed; ROLLBACK");
            for (name, entries) in engine.durable.sql_user_functions.read().iter() {
                for (value, old) in entries.iter().zip(&original[name]) {
                    assert_eq!(
                        serde_json::to_value(&value.def).unwrap(),
                        serde_json::to_value(&old.def).unwrap()
                    );
                }
            }
            sql(&engine,"DROP FUNCTION bound(integer); DROP FUNCTION bound(text); DROP FUNCTION deferred(); DROP PROCEDURE change(); DROP TABLE docs");
            fixture(&engine);
            assert_ne!(
                engine.durable.sql_user_functions.read()["public.bound"][0]
                    .def
                    .object_id,
                original["public.bound"][0].def.object_id
            );
        }
        drop(engine);
        let saved = key.retained(&cut).unwrap().unwrap();
        same(saved.get().unwrap(), &original);
        drop(cut);
        control.cancellation().cancel();
        assert!(matches!(
            saved.get(),
            Err(StorageBackendError::Cancelled(_))
        ));
        drop(saved);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn denied_routine_capture_preserves_ddl_and_independently_funded_source_bodies() {
    let engine = Engine::new();
    fixture(&engine);
    sql(
        &engine,
        &format!(
            "CREATE FUNCTION large() RETURNS text LANGUAGE SQL AS 'SELECT ''{}'''",
            "x".repeat(65536)
        ),
    );
    let key = engine.durable.sql_user_functions.key().unwrap();
    let original = engine.durable.sql_user_functions.snapshot();
    let small = StorageReadControl::with_limit(4096);
    let good = StorageReadControl::with_limit(1 << 20);
    let registry = engine.storage.memory_capture.as_ref().unwrap();
    let failed = registry.capture(&small).unwrap();
    let cut = registry.capture(&good).unwrap();
    sql(&engine,"DROP FUNCTION large(); CREATE OR REPLACE FUNCTION bound(a integer DEFAULT 7) RETURNS integer LANGUAGE SQL RETURN a + 5");
    assert!(failed.check().is_err());
    assert!(!engine
        .durable
        .sql_user_functions
        .read()
        .contains_key("public.large"));
    drop(engine);
    let saved = key.retained(&cut).unwrap().unwrap();
    same(saved.get().unwrap(), &original);
    drop((saved, cut, failed));
    assert_eq!(small.memory().used(), 0);
    assert_eq!(good.memory().used(), 0);
}
