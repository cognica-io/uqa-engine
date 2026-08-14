//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! UPDATE and DELETE round-trips with WHERE filtering.

use uqa_core::Value;
use uqa_engine::Engine;
use uqa_sql::ColumnType;

fn corpus() -> Engine {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT, qty INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO items (id, label, qty) VALUES \
         (1, 'apple', 3), (2, 'banana', 7), (3, 'cherry', 2), (4, 'date', 5)",
        &[],
    )
    .unwrap();
    eng
}

#[test]
fn update_changes_matched_rows() {
    let eng = corpus();
    let r = eng
        .sql("UPDATE items SET qty = 99 WHERE label = 'banana'", &[])
        .unwrap();
    assert_eq!(r.affected_rows, 1);
    let r = eng
        .sql("SELECT label, qty FROM items WHERE label = 'banana'", &[])
        .unwrap();
    assert_eq!(r.rows.len(), 1);
    assert_eq!(r.rows[0].get("qty"), Some(&Value::Int(99)));
}

#[test]
fn update_with_arithmetic_expression() {
    let eng = corpus();
    let r = eng
        .sql("UPDATE items SET qty = qty + 1 WHERE qty < 5", &[])
        .unwrap();
    assert_eq!(r.affected_rows, 2); // apple, cherry

    let r = eng
        .sql("SELECT label, qty FROM items ORDER BY label", &[])
        .unwrap();
    assert_eq!(r.rows.len(), 4);
    let map: std::collections::BTreeMap<_, _> = r
        .rows
        .iter()
        .filter_map(|row| match (row.get("label"), row.get("qty")) {
            (Some(Value::Str(l)), Some(Value::Int(n))) => Some((l.clone(), *n)),
            _ => None,
        })
        .collect();
    assert_eq!(map.get("apple"), Some(&4));
    assert_eq!(map.get("cherry"), Some(&3));
    assert_eq!(map.get("banana"), Some(&7));
    assert_eq!(map.get("date"), Some(&5));
}

#[test]
fn delete_removes_matched_rows() {
    let eng = corpus();
    let r = eng.sql("DELETE FROM items WHERE qty <= 3", &[]).unwrap();
    assert_eq!(r.affected_rows, 2);
    let r = eng.sql("SELECT count(*) AS n FROM items", &[]).unwrap();
    assert_eq!(r.rows[0].get("n"), Some(&Value::Int(2)));
}

#[test]
fn delete_without_where_truncates_table() {
    let eng = corpus();
    let r = eng.sql("DELETE FROM items", &[]).unwrap();
    assert_eq!(r.affected_rows, 4);
    let r = eng.sql("SELECT count(*) AS n FROM items", &[]).unwrap();
    assert_eq!(r.rows[0].get("n"), Some(&Value::Int(0)));
}

#[test]
fn delete_filter_errors_propagate_and_roll_back() {
    let eng = corpus();
    let error = eng
        .sql(
            "WITH marker AS (SELECT 1) \
             DELETE FROM items WHERE qty / (qty - qty) > 0",
            &[],
        )
        .unwrap_err();
    assert!(error.to_string().contains("division by zero"));

    let result = eng.sql("SELECT count(*) AS n FROM items", &[]).unwrap();
    assert_eq!(result.rows[0].get("n"), Some(&Value::Int(4)));
}

#[test]
fn dml_scalar_subqueries_execute_query_plan_children() {
    let eng = corpus();

    let updated = eng
        .sql(
            "UPDATE items SET qty = (SELECT max(qty) FROM items) WHERE id = 1",
            &[],
        )
        .unwrap();
    assert_eq!(updated.affected_rows, 1);

    let inserted = eng
        .sql(
            "INSERT INTO items (id, label, qty) \
             VALUES (5, 'elderberry', (SELECT max(qty) FROM items))",
            &[],
        )
        .unwrap();
    assert_eq!(inserted.affected_rows, 1);

    let deleted = eng
        .sql(
            "DELETE FROM items WHERE qty < (SELECT max(qty) FROM items)",
            &[],
        )
        .unwrap();
    assert_eq!(deleted.affected_rows, 2);

    let result = eng
        .sql("SELECT id, qty FROM items ORDER BY id", &[])
        .unwrap();
    assert_eq!(result.rows.len(), 3);
    assert!(result
        .rows
        .iter()
        .all(|row| row.get("qty") == Some(&Value::Int(7))));
}

#[test]
fn postgresql_18_returning_exposes_old_and_new_row_images() {
    let eng = corpus();

    let inserted = eng
        .sql(
            "INSERT INTO items (id, label, qty) VALUES (5, 'elderberry', 11) \
             RETURNING old.id IS NULL AS old_missing, new.id AS inserted_id",
            &[],
        )
        .unwrap();
    assert_eq!(
        inserted.rows[0].get("old_missing"),
        Some(&Value::Bool(true))
    );
    assert_eq!(inserted.rows[0].get("inserted_id"), Some(&Value::Int(5)));

    let updated = eng
        .sql(
            "UPDATE items SET qty = qty + 1 WHERE id = 2 \
             RETURNING WITH (OLD AS before, NEW AS after) \
             id AS current_id, before.qty AS old_qty, after.qty AS new_qty",
            &[],
        )
        .unwrap();
    assert_eq!(updated.rows[0].get("old_qty"), Some(&Value::Int(7)));
    assert_eq!(updated.rows[0].get("new_qty"), Some(&Value::Int(8)));
    assert_eq!(updated.rows[0].get("current_id"), Some(&Value::Int(2)));

    let deleted = eng
        .sql(
            "DELETE FROM items WHERE id = 3 \
             RETURNING old.label AS deleted_label, new.id IS NULL AS new_missing",
            &[],
        )
        .unwrap();
    assert_eq!(
        deleted.rows[0].get("deleted_label"),
        Some(&Value::Str("cherry".into()))
    );
    assert_eq!(deleted.rows[0].get("new_missing"), Some(&Value::Bool(true)));
}

#[test]
fn returning_preserves_declared_types_for_rows_and_empty_results() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE typed_dml (
            id SMALLINT PRIMARY KEY,
            label VARCHAR(7),
            amount REAL
        )",
        &[],
    )
    .unwrap();

    let inserted = eng
        .sql(
            "INSERT INTO typed_dml VALUES (1, 'one', 1.5)
             RETURNING *",
            &[],
        )
        .unwrap();
    assert_eq!(inserted.columns, ["id", "label", "amount"]);
    assert_eq!(
        inserted.column_types,
        [
            Some(ColumnType::SmallInteger),
            Some(ColumnType::Varchar(Some(7))),
            Some(ColumnType::Real),
        ]
    );
    assert_eq!(inserted.rows[0].len(), 3);

    let updated = eng
        .sql(
            "UPDATE typed_dml SET amount = amount WHERE FALSE
             RETURNING old.label AS old_label, new.amount AS new_amount",
            &[],
        )
        .unwrap();
    assert!(updated.rows.is_empty());
    assert_eq!(
        updated.column_types,
        [Some(ColumnType::Varchar(Some(7))), Some(ColumnType::Real),]
    );

    let deleted = eng
        .sql(
            "DELETE FROM typed_dml WHERE FALSE
             RETURNING old.id AS old_id, new.id IS NULL AS new_missing",
            &[],
        )
        .unwrap();
    assert!(deleted.rows.is_empty());
    assert_eq!(
        deleted.column_types,
        [Some(ColumnType::SmallInteger), Some(ColumnType::Boolean),]
    );
}
