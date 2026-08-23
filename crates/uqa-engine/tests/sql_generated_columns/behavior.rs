//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generated-column DML and ALTER behavior.

use super::*;

#[test]
fn generated_columns_follow_pg18_insert_update_and_read_semantics() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE generated_rows (
                 id INTEGER PRIMARY KEY,
                 source INTEGER,
                 virtual_value INTEGER GENERATED ALWAYS AS (source + 1),
                 stored_value INTEGER GENERATED ALWAYS AS (source * 2) STORED
             )",
            &[],
        )
        .unwrap();

    let inserted = engine
        .sql(
            "INSERT INTO generated_rows VALUES (1, 4, DEFAULT, DEFAULT) RETURNING virtual_value, stored_value",
            &[],
        )
        .unwrap();
    assert_eq!(int(&inserted.rows[0], "virtual_value"), 5);
    assert_eq!(int(&inserted.rows[0], "stored_value"), 8);

    engine
        .sql("INSERT INTO generated_rows VALUES (2, 5)", &[])
        .unwrap();
    let error = engine
        .sql("INSERT INTO generated_rows VALUES (3, 6, 99, DEFAULT)", &[])
        .unwrap_err()
        .to_string();
    assert!(error.contains("only DEFAULT may be assigned"), "{error}");

    let updated = engine
        .sql(
            "UPDATE generated_rows SET source = 10, virtual_value = DEFAULT, stored_value = DEFAULT WHERE id = 1 RETURNING virtual_value, stored_value",
            &[],
        )
        .unwrap();
    assert_eq!(int(&updated.rows[0], "virtual_value"), 11);
    assert_eq!(int(&updated.rows[0], "stored_value"), 20);
    let error = engine
        .sql(
            "UPDATE generated_rows SET stored_value = 7 WHERE id = 1",
            &[],
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("only DEFAULT may be assigned"), "{error}");

    let selected = engine
        .sql(
            "SELECT id, virtual_value, stored_value FROM generated_rows WHERE virtual_value >= 6 ORDER BY virtual_value DESC",
            &[],
        )
        .unwrap();
    assert_eq!(selected.rows.len(), 2);
    assert_eq!(int(&selected.rows[0], "id"), 1);
    assert_eq!(int(&selected.rows[1], "id"), 2);

    let equality = engine
        .sql("SELECT id FROM generated_rows WHERE virtual_value = 6", &[])
        .unwrap();
    assert_eq!(equality.rows.len(), 1);
    assert_eq!(int(&equality.rows[0], "id"), 2);
}

#[test]
fn implicit_insert_slots_keep_generated_columns_in_declared_order() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE generated_slots (
                 source INTEGER,
                 derived INTEGER GENERATED ALWAYS AS (source * 10),
                 tail INTEGER
             )",
            &[],
        )
        .unwrap();
    engine
        .sql("INSERT INTO generated_slots VALUES (1, DEFAULT, 2)", &[])
        .unwrap();
    engine
        .sql("INSERT INTO generated_slots VALUES (3)", &[])
        .unwrap();
    let error = engine
        .sql("INSERT INTO generated_slots VALUES (4, 5)", &[])
        .unwrap_err()
        .to_string();
    assert!(error.contains("only DEFAULT may be assigned"), "{error}");
    let select_error = engine
        .sql("INSERT INTO generated_slots SELECT 6, 7", &[])
        .unwrap_err()
        .to_string();
    assert!(
        select_error.contains("only DEFAULT may be assigned"),
        "{select_error}"
    );
    engine
        .sql(
            "INSERT INTO generated_slots (source, tail) SELECT 8, 9",
            &[],
        )
        .unwrap();

    let result = engine
        .sql(
            "SELECT source, derived, tail FROM generated_slots ORDER BY source",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows.len(), 3);
    assert_eq!(int(&result.rows[0], "derived"), 10);
    assert_eq!(result.rows[1].get("tail"), Some(&Value::Null));
    assert_eq!(int(&result.rows[2], "derived"), 80);
    assert_eq!(int(&result.rows[2], "tail"), 9);
}

#[test]
fn alter_generated_expression_rewrites_stored_rows_and_preserves_drop_value() {
    let engine = Engine::new();
    engine
        .sql("CREATE TABLE altered_generated (source INTEGER)", &[])
        .unwrap();
    engine
        .sql("INSERT INTO altered_generated VALUES (3), (5)", &[])
        .unwrap();
    engine
        .sql(
            "ALTER TABLE altered_generated ADD COLUMN stored_value INTEGER GENERATED ALWAYS AS (source * 2) STORED",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "ALTER TABLE altered_generated ADD COLUMN virtual_value INTEGER GENERATED ALWAYS AS (source + 1)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "ALTER TABLE altered_generated ALTER COLUMN stored_value SET EXPRESSION AS (source * 3)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "ALTER TABLE altered_generated ALTER COLUMN virtual_value SET EXPRESSION AS (source + 4)",
            &[],
        )
        .unwrap();
    let result = engine
        .sql(
            "SELECT source, stored_value, virtual_value FROM altered_generated ORDER BY source",
            &[],
        )
        .unwrap();
    assert_eq!(int(&result.rows[0], "stored_value"), 9);
    assert_eq!(int(&result.rows[0], "virtual_value"), 7);
    assert_eq!(int(&result.rows[1], "stored_value"), 15);
    assert_eq!(int(&result.rows[1], "virtual_value"), 9);

    let virtual_drop = engine
        .sql(
            "ALTER TABLE altered_generated ALTER COLUMN virtual_value DROP EXPRESSION",
            &[],
        )
        .unwrap_err()
        .to_string();
    assert!(virtual_drop.contains("virtual generated"), "{virtual_drop}");
    engine
        .sql(
            "ALTER TABLE altered_generated ALTER COLUMN stored_value DROP EXPRESSION",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "UPDATE altered_generated SET source = 10 WHERE source = 3",
            &[],
        )
        .unwrap();
    let retained = engine
        .sql(
            "SELECT stored_value FROM altered_generated WHERE source = 10",
            &[],
        )
        .unwrap();
    assert_eq!(int(&retained.rows[0], "stored_value"), 9);
}
