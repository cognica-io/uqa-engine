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
fn explicit_returning_row_aliases_cannot_shadow_query_relations() {
    let eng = corpus();
    eng.sql(
        "CREATE TABLE adjustments (id INTEGER PRIMARY KEY, qty INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO adjustments VALUES (2, 99)", &[])
        .unwrap();

    let source_error = eng
        .sql(
            "UPDATE items AS target SET qty = adjustments.qty
             FROM adjustments AS adjustments
             WHERE target.id = adjustments.id
             RETURNING WITH (OLD AS adjustments, NEW AS after) after.id",
            &[],
        )
        .unwrap_err();
    assert_eq!(source_error.sqlstate(), Some("42712"));
    assert!(source_error
        .to_string()
        .contains("table name \"adjustments\" specified more than once"));

    let target_error = eng
        .sql(
            "UPDATE items AS target SET qty = 100 WHERE id = 2
             RETURNING WITH (OLD AS target, NEW AS after) after.id",
            &[],
        )
        .unwrap_err();
    assert_eq!(target_error.sqlstate(), Some("42712"));
    assert!(target_error
        .to_string()
        .contains("table name \"target\" specified more than once"));

    let unchanged = eng.sql("SELECT qty FROM items WHERE id = 2", &[]).unwrap();
    assert_eq!(unchanged.rows[0]["qty"], Value::Int(7));
}

#[test]
fn returning_old_and_new_stars_preserve_duplicate_output_positions() {
    let eng = corpus();
    let result = eng
        .sql(
            "UPDATE items SET label = 'ripe banana', qty = qty + 1 WHERE id = 2
             RETURNING old.*, new.*",
            &[],
        )
        .unwrap();

    assert_eq!(result.columns, ["id", "label", "qty", "id", "label", "qty"]);
    assert_eq!(result.value_at(0, 0), Some(&Value::Int(2)));
    assert_eq!(result.value_at(0, 1), Some(&Value::Str("banana".into())));
    assert_eq!(result.value_at(0, 2), Some(&Value::Int(7)));
    assert_eq!(result.value_at(0, 3), Some(&Value::Int(2)));
    assert_eq!(
        result.value_at(0, 4),
        Some(&Value::Str("ripe banana".into()))
    );
    assert_eq!(result.value_at(0, 5), Some(&Value::Int(8)));
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

#[test]
fn dml_target_aliases_and_relation_qualifiers_are_structural() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE \"a.b\" (id INTEGER PRIMARY KEY, \"c.d\" INTEGER, prior INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO \"a.b\" VALUES (1, 10, 0)", &[])
        .unwrap();

    let updated = eng
        .sql(
            "UPDATE \"a.b\" AS target
             SET \"c.d\" = target.\"c.d\" + 1, prior = target.\"c.d\"
             WHERE target.id = 1
             RETURNING target.id, old.\"c.d\" AS before, new.\"c.d\" AS after, prior",
            &[],
        )
        .unwrap();
    assert_eq!(updated.rows[0].get("id"), Some(&Value::Int(1)));
    assert_eq!(updated.rows[0].get("before"), Some(&Value::Int(10)));
    assert_eq!(updated.rows[0].get("after"), Some(&Value::Int(11)));
    assert_eq!(updated.rows[0].get("prior"), Some(&Value::Int(10)));

    let hidden_name_error = eng
        .sql(
            "UPDATE \"a.b\" AS target SET prior = prior WHERE \"a.b\".id = 1",
            &[],
        )
        .unwrap_err();
    assert!(hidden_name_error.to_string().contains("a.b"));

    let inserted = eng
        .sql(
            "INSERT INTO \"a.b\" AS target VALUES (2, 20, 0)
             RETURNING target.id, old.id IS NULL AS old_missing, new.\"c.d\"",
            &[],
        )
        .unwrap();
    assert_eq!(inserted.rows[0].get("id"), Some(&Value::Int(2)));
    assert_eq!(
        inserted.rows[0].get("old_missing"),
        Some(&Value::Bool(true))
    );
    assert_eq!(inserted.rows[0].get("c.d"), Some(&Value::Int(20)));

    let deleted = eng
        .sql(
            "DELETE FROM \"a.b\" AS target WHERE target.id = 2
             RETURNING target.\"c.d\", old.id, new.id IS NULL AS new_missing",
            &[],
        )
        .unwrap();
    assert_eq!(deleted.rows[0].get("c.d"), Some(&Value::Int(20)));
    assert_eq!(deleted.rows[0].get("id"), Some(&Value::Int(2)));
    assert_eq!(deleted.rows[0].get("new_missing"), Some(&Value::Bool(true)));
}

#[test]
fn schema_qualified_dml_binds_the_local_relation_name() {
    let eng = Engine::new();
    eng.sql("CREATE SCHEMA app", &[]).unwrap();
    eng.sql(
        "CREATE TABLE app.items (id INTEGER PRIMARY KEY, value INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO app.items VALUES (1, 5)", &[]).unwrap();

    let result = eng
        .sql(
            "UPDATE app.items SET value = items.value + 1 WHERE items.id = 1
             RETURNING items.id, old.value, new.value",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows[0].get("id"), Some(&Value::Int(1)));
    assert_eq!(result.rows[0].get("value"), Some(&Value::Int(5)));
    assert_eq!(result.rows[0].get("value_1"), Some(&Value::Int(6)));
}

#[test]
fn update_fast_path_never_bypasses_enforced_check_constraints() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE checked_update (id INTEGER PRIMARY KEY, value INTEGER CHECK (value > 0))",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO checked_update VALUES (1, 1)", &[])
        .unwrap();

    let error = eng
        .sql("UPDATE checked_update SET value = -1 WHERE id = 1", &[])
        .unwrap_err();
    assert!(error.to_string().contains("CHECK constraint"), "{error}");
    let result = eng
        .sql("SELECT value FROM checked_update WHERE id = 1", &[])
        .unwrap();
    assert_eq!(result.rows[0].get("value"), Some(&Value::Int(1)));
}

#[test]
fn update_expressions_preserve_declared_integer_widths() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE typed_update (id INTEGER PRIMARY KEY, small_value SMALLINT, bytes BYTEA, big_value BIGINT)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO typed_update VALUES (1, -1, NULL, 3000000000)",
        &[],
    )
    .unwrap();

    eng.sql(
        "UPDATE typed_update SET bytes = small_value::bytea, big_value = -big_value WHERE id = 1",
        &[],
    )
    .unwrap();
    let result = eng
        .sql(
            "SELECT bytes, big_value FROM typed_update WHERE id = 1",
            &[],
        )
        .unwrap();
    assert_eq!(
        result.rows[0].get("bytes"),
        Some(&Value::Bytes(vec![0xff, 0xff]))
    );
    assert_eq!(
        result.rows[0].get("big_value"),
        Some(&Value::Int(-3_000_000_000))
    );
}
