//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A foreign key that references a table which is not partitioned reads only that table's own rows, as `PostgreSQL`'s referential queries read `ONLY` the table: the rows of its plain inheritance children neither satisfy nor are touched by it.

use super::{exec, Engine, Value};

fn fixture(engine: &Engine) {
    exec(
        engine,
        "CREATE TABLE inherited_keys (a integer PRIMARY KEY)",
    );
    exec(
        engine,
        "CREATE TABLE inherited_keys_child () INHERITS (inherited_keys)",
    );
    exec(engine, "INSERT INTO inherited_keys VALUES (6)");
    exec(engine, "INSERT INTO inherited_keys_child VALUES (5)");
    exec(
        engine,
        "CREATE TABLE inherited_key_refs (a integer REFERENCES inherited_keys ON DELETE CASCADE ON UPDATE CASCADE)",
    );
    exec(engine, "INSERT INTO inherited_key_refs VALUES (6)");
}

fn count(engine: &Engine, sql: &str) -> Value {
    engine.sql(sql, &[]).unwrap().rows[0]
        .values()
        .next()
        .unwrap()
        .clone()
}

#[test]
fn a_key_held_only_by_an_inheritance_child_is_not_present_in_the_referenced_table() {
    let engine = Engine::new();
    fixture(&engine);
    let error = engine
        .sql("INSERT INTO inherited_key_refs VALUES (5)", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("23503"), "{error}");
    assert_eq!(
        count(&engine, "SELECT count(*) FROM inherited_key_refs"),
        Value::Int(1)
    );
}

#[test]
fn changing_an_inheritance_child_row_leaves_the_rows_that_reference_its_parent() {
    let engine = Engine::new();
    fixture(&engine);
    // The child holds a row with the referenced key; the parent's own row is the one referenced.
    exec(&engine, "INSERT INTO inherited_keys_child VALUES (6)");
    exec(&engine, "UPDATE inherited_keys_child SET a = 7 WHERE a = 6");
    exec(&engine, "DELETE FROM inherited_keys_child WHERE a = 7");
    assert_eq!(
        count(
            &engine,
            "SELECT count(*) FROM inherited_key_refs WHERE a = 6"
        ),
        Value::Int(1)
    );
    // Deleting the parent's own row still cascades.
    exec(&engine, "DELETE FROM ONLY inherited_keys WHERE a = 6");
    assert_eq!(
        count(&engine, "SELECT count(*) FROM inherited_key_refs"),
        Value::Int(0)
    );
}
