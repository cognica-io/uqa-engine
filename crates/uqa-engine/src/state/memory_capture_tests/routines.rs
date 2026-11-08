//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{sql, Engine, StorageReadControl};
use uqa_sql::{
    routines::registry::CapturedRoutines,
    routines::{CompiledFunctionBody, RoutineBody},
};
use uqa_storage::StorageBackendError;

fn fixture(engine: &Engine) {
    sql(engine,"CREATE TABLE docs(id integer); CREATE FUNCTION bound(a integer DEFAULT 7) RETURNS integer LANGUAGE SQL STABLE SECURITY DEFINER RETURN a + 1; CREATE FUNCTION bound(a text) RETURNS text LANGUAGE SQL AS 'SELECT a'; CREATE PROCEDURE change() LANGUAGE SQL BEGIN ATOMIC INSERT INTO docs VALUES (1); END; SET check_function_bodies = off; CREATE FUNCTION deferred() RETURNS integer LANGUAGE plpgsql AS 'invalid until invoked'; RESET check_function_bodies");
}

#[test]
fn memory_routine_images_preserve_published_bindings_after_engine_retirement() {
    let engine = Engine::new();
    fixture(&engine);
    sql(
        &engine,
        "CREATE ROLE reader; GRANT EXECUTE ON FUNCTION bound(integer) TO reader",
    );
    let original = engine.durable.sql_user_functions.snapshot();
    let key = engine.durable.sql_user_functions.key().unwrap();
    let control = StorageReadControl::with_limit(4 << 20);
    let cut = engine
        .storage
        .memory_capture
        .as_ref()
        .unwrap()
        .capture(&control)
        .unwrap();
    sql(&engine, "CREATE OR REPLACE FUNCTION bound(a integer DEFAULT 9) RETURNS integer LANGUAGE SQL RETURN a + 100; REVOKE EXECUTE ON FUNCTION bound(integer) FROM reader; DROP PROCEDURE change()");
    drop(engine);
    let saved = key.retained(&cut).unwrap().unwrap();
    drop(cut);
    let memory = uqa_core::memory::MemoryBudget::new(4 << 20);
    let token = uqa_core::CancellationToken::new();
    let mut reader = saved.get().unwrap().image_reader(&memory, &token).unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0; 23];
    loop {
        let count = reader.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    drop(reader);
    drop(saved);
    assert_eq!(control.memory().used(), 0);
    assert_eq!(memory.used(), 0);
    let restored = uqa_sql::routines::registry::RoutineRegistry::read_image(
        &mut bytes.as_slice(),
        &memory,
        &token,
    )
    .unwrap();
    let captured_memory = uqa_core::memory::MemoryBudget::new(4 << 20);
    let captured = restored
        .capture(&uqa_core::memory::ProductionControl::new(
            &captured_memory,
            &token,
            &token,
        ))
        .unwrap();
    same(&captured, &original);
    drop(original);
    let RoutineBody::Bound(body) = &restored["public.bound"][0].body else {
        panic!("bound body")
    };
    let body = body.clone();
    drop(restored);
    drop(captured);
    assert_eq!(captured_memory.used(), 0);
    assert!(memory.used() > 0);
    std::thread::spawn(move || {
        let CompiledFunctionBody::SQL(plans) = &**body else {
            panic!("SQL body")
        };
        assert_eq!(plans.len(), 1);
        drop(body);
    })
    .join()
    .unwrap();
    assert_eq!(memory.used(), 0);
}

fn same(saved: &CapturedRoutines, original: &uqa_sql::routines::registry::RoutineRegistry) {
    assert_eq!(saved.len(), original.len());
    for ((name, entries), (old_name, old)) in saved.iter().zip(original) {
        assert_eq!(name, old_name);
        assert_eq!(entries.len(), old.len());
        for (value, old) in entries.iter().zip(old) {
            assert_eq!(
                serde_json::to_value(&value.def).unwrap(),
                serde_json::to_value(&old.def).unwrap()
            );
            match (&value.body, &old.body) {
                (RoutineBody::Source, RoutineBody::Source) => {}
                (RoutineBody::Bound(plans), RoutineBody::Bound(body)) => {
                    let CompiledFunctionBody::SQL(old) = &***body else {
                        panic!("SQL")
                    };
                    let CompiledFunctionBody::SQL(plans) = &***plans else {
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
