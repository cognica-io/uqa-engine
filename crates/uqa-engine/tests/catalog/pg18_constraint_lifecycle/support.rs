//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared setup and constraint assertions for the existing lifecycle test modules.

use uqa_core::Value;
use uqa_engine::Engine;

pub(super) fn exec(engine: &Engine, sql: &str) {
    engine.sql(sql, &[]).unwrap_or_else(|error| {
        panic!("SQL failed: {sql}\n{error:?}");
    });
}

pub(super) fn error(engine: &Engine, sql: &str, state: &str, message: &str) {
    let error = engine.sql(sql, &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some(state), "{sql}: {error}");
    assert!(error.to_string().contains(message), "{sql}: {error}");
}

pub(super) fn bools(engine: &Engine, name: &str) -> (bool, bool, bool, bool, bool) {
    let result = engine
        .sql(
            &format!(
                "SELECT condeferrable, condeferred, conenforced, convalidated, connoinherit FROM pg_catalog.pg_constraint WHERE conname = '{name}'"
            ),
            &[],
        )
        .unwrap();
    let row = result
        .rows
        .first()
        .unwrap_or_else(|| panic!("missing constraint {name}"));
    let read = |column: &str| match row.get(column) {
        Some(Value::Bool(value)) => *value,
        value => panic!("unexpected {column} for {name}: {value:?}"),
    };
    (
        read("condeferrable"),
        read("condeferred"),
        read("conenforced"),
        read("convalidated"),
        read("connoinherit"),
    )
}

pub(super) fn constraint_exists(engine: &Engine, name: &str) -> bool {
    !engine
        .sql(
            &format!("SELECT conname FROM pg_catalog.pg_constraint WHERE conname = '{name}'"),
            &[],
        )
        .unwrap()
        .rows
        .is_empty()
}

pub(super) fn deferred_event_engine() -> Engine {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE root (id INTEGER PRIMARY KEY)");
    exec(&engine, "CREATE TABLE parent_a (id INTEGER PRIMARY KEY)");
    exec(&engine, "CREATE TABLE parent_b (id INTEGER PRIMARY KEY)");
    exec(
        &engine,
        "CREATE TABLE child (id INTEGER PRIMARY KEY, a_id INTEGER, b_id INTEGER, marker INTEGER, CONSTRAINT child_a_fk FOREIGN KEY (a_id) REFERENCES parent_a(id) DEFERRABLE INITIALLY DEFERRED)",
    );
    exec(&engine, "INSERT INTO parent_a VALUES (1)");
    exec(&engine, "INSERT INTO child VALUES (1, 1, 999, 0)");
    exec(
        &engine,
        "ALTER TABLE child ADD CONSTRAINT child_b_fk FOREIGN KEY (b_id) REFERENCES parent_b(id) DEFERRABLE INITIALLY IMMEDIATE NOT VALID",
    );
    engine
}
