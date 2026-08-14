//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL join coverage.

use std::collections::BTreeSet;

use uqa_core::Value;
use uqa_engine::Engine;

fn exec(engine: &Engine, sql: &str) {
    engine.sql(sql, &[]).unwrap();
}

fn query(engine: &Engine, sql: &str) -> uqa_sql::SQLResult {
    engine.sql(sql, &[]).unwrap()
}

fn engine_with_orders() -> Engine {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)",
    );
    exec(&engine, "INSERT INTO users (id, name) VALUES (1, 'Alice')");
    exec(&engine, "INSERT INTO users (id, name) VALUES (2, 'Bob')");
    exec(&engine, "INSERT INTO users (id, name) VALUES (3, 'Carol')");
    exec(
        &engine,
        "CREATE TABLE orders (oid INTEGER PRIMARY KEY, user_id INTEGER, product TEXT)",
    );
    exec(
        &engine,
        "INSERT INTO orders (oid, user_id, product) VALUES (10, 1, 'Book')",
    );
    exec(
        &engine,
        "INSERT INTO orders (oid, user_id, product) VALUES (11, 1, 'Pen')",
    );
    exec(
        &engine,
        "INSERT INTO orders (oid, user_id, product) VALUES (12, 2, 'Notebook')",
    );
    engine
}

fn lateral_engine() -> Engine {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE depts (id INT PRIMARY KEY, dept_name TEXT)",
    );
    exec(
        &engine,
        "CREATE TABLE emps (id INT PRIMARY KEY, emp_name TEXT, dept_id INT, salary INT)",
    );
    exec(&engine, "INSERT INTO depts VALUES (1, 'Engineering')");
    exec(&engine, "INSERT INTO depts VALUES (2, 'Sales')");
    exec(&engine, "INSERT INTO emps VALUES (1, 'Alice', 1, 90000)");
    exec(&engine, "INSERT INTO emps VALUES (2, 'Bob', 1, 80000)");
    exec(&engine, "INSERT INTO emps VALUES (3, 'Charlie', 2, 70000)");
    exec(&engine, "INSERT INTO emps VALUES (4, 'Diana', 2, 75000)");
    engine
}

fn using_engine() -> Engine {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE join_left (id INTEGER, shared TEXT, l_only TEXT)",
    );
    exec(
        &engine,
        "CREATE TABLE join_right (id INTEGER, shared TEXT, r_only TEXT)",
    );
    exec(
        &engine,
        "INSERT INTO join_left VALUES
            (1, 'same', 'l1'),
            (2, 'left', 'l2'),
            (NULL, 'null-left', 'ln')",
    );
    exec(
        &engine,
        "INSERT INTO join_right VALUES
            (1, 'same', 'r1'),
            (3, 'right', 'r3'),
            (NULL, 'null-right', 'rn')",
    );
    engine
}

fn str_set(result: &uqa_sql::SQLResult, column: &str) -> BTreeSet<String> {
    result
        .rows
        .iter()
        .filter_map(|r| match r.get(column) {
            Some(Value::Str(s)) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn inner_join_basic() {
    let engine = engine_with_orders();
    let result = query(
        &engine,
        "SELECT users.name, orders.product \
         FROM users INNER JOIN orders ON users.id = orders.user_id",
    );
    assert_eq!(result.rows.len(), 3);
    assert_eq!(
        str_set(&result, "product"),
        ["Book", "Notebook", "Pen"]
            .into_iter()
            .map(String::from)
            .collect()
    );
}

#[test]
fn comma_join_column_pruning_keeps_only_real_source_columns() {
    let engine = engine_with_orders();
    let result = query(
        &engine,
        "SELECT name, product
         FROM users, orders
         WHERE id = user_id
         ORDER BY oid",
    );

    assert_eq!(result.rows.len(), 3);
    assert_eq!(result.rows[0]["name"], Value::Str("Alice".into()));
    assert_eq!(result.rows[0]["product"], Value::Str("Book".into()));
    assert_eq!(result.rows[2]["name"], Value::Str("Bob".into()));
    assert_eq!(result.rows[2]["product"], Value::Str("Notebook".into()));
}

#[test]
fn inner_join_excludes_unmatched() {
    let engine = engine_with_orders();
    let result = query(
        &engine,
        "SELECT users.name \
         FROM users INNER JOIN orders ON users.id = orders.user_id",
    );
    assert!(!str_set(&result, "name").contains("Carol"));
}

#[test]
fn inner_join_uses_composite_expression_key() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE points (id INTEGER PRIMARY KEY, x REAL, y REAL)",
    );
    exec(
        &engine,
        "CREATE TABLE tiles (x INTEGER, y INTEGER, label TEXT)",
    );
    // Coordinates chosen so PostgreSQL 18 float -> int casts (round
    // half to even, not truncation) land on the intended tiles.
    exec(
        &engine,
        "INSERT INTO points (id, x, y) VALUES
            (1, 1.2, 2.4),
            (2, 4.1, 5.0),
            (3, 9.9, 9.9)",
    );
    exec(
        &engine,
        "INSERT INTO tiles (x, y, label) VALUES
            (1, 2, 'wall'),
            (4, 5, 'floor'),
            (9, 8, 'miss')",
    );

    let result = query(
        &engine,
        "SELECT p.id, t.label
         FROM points p
         JOIN tiles t
           ON t.x = CAST(p.x AS INT)
          AND t.y = CAST(p.y AS INT)
         ORDER BY p.id",
    );

    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.rows[0]["label"], Value::Str("wall".into()));
    assert_eq!(result.rows[1]["label"], Value::Str("floor".into()));
}

#[test]
fn left_join_uses_composite_expression_key_and_pads_unmatched() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE points (id INTEGER PRIMARY KEY, x REAL, y REAL)",
    );
    exec(
        &engine,
        "CREATE TABLE tiles (x INTEGER, y INTEGER, label TEXT)",
    );
    // Coordinates chosen so PostgreSQL 18 float -> int casts (round
    // half to even, not truncation) land on the intended tiles.
    exec(
        &engine,
        "INSERT INTO points (id, x, y) VALUES
            (1, 1.2, 2.4),
            (2, 4.1, 5.0),
            (3, 9.9, 9.9)",
    );
    exec(
        &engine,
        "INSERT INTO tiles (x, y, label) VALUES
            (1, 2, 'wall'),
            (4, 5, 'floor')",
    );

    let result = query(
        &engine,
        "SELECT p.id, t.label
         FROM points p
         LEFT JOIN tiles t
           ON t.x = CAST(p.x AS INT)
          AND t.y = CAST(p.y AS INT)
         ORDER BY p.id",
    );

    assert_eq!(result.rows.len(), 3);
    assert_eq!(result.rows[0]["label"], Value::Str("wall".into()));
    assert_eq!(result.rows[1]["label"], Value::Str("floor".into()));
    assert_eq!(result.rows[2]["label"], Value::Null);
}

#[test]
fn left_join_preserves_left() {
    let engine = engine_with_orders();
    let result = query(
        &engine,
        "SELECT users.name, orders.product \
         FROM users LEFT JOIN orders ON users.id = orders.user_id",
    );
    assert_eq!(result.rows.len(), 4);
    assert!(str_set(&result, "name").contains("Carol"));
}

#[test]
fn left_join_null_for_unmatched() {
    let engine = engine_with_orders();
    let result = query(
        &engine,
        "SELECT users.name, orders.product \
         FROM users LEFT JOIN orders ON users.id = orders.user_id",
    );
    let carol: Vec<_> = result
        .rows
        .iter()
        .filter(|r| r.get("name") == Some(&Value::Str("Carol".into())))
        .collect();
    assert_eq!(carol.len(), 1);
    assert_eq!(carol[0].get("product"), Some(&Value::Null));
}

#[test]
fn cross_join_cartesian() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE a (id INTEGER PRIMARY KEY, val TEXT)");
    exec(&engine, "INSERT INTO a (id, val) VALUES (1, 'x')");
    exec(&engine, "INSERT INTO a (id, val) VALUES (2, 'y')");
    exec(
        &engine,
        "CREATE TABLE b (id INTEGER PRIMARY KEY, label TEXT)",
    );
    exec(&engine, "INSERT INTO b (id, label) VALUES (10, 'p')");
    exec(&engine, "INSERT INTO b (id, label) VALUES (20, 'q')");
    exec(&engine, "INSERT INTO b (id, label) VALUES (30, 'r')");
    let result = query(&engine, "SELECT a.val, b.label FROM a CROSS JOIN b");
    assert_eq!(result.rows.len(), 6);
}

#[test]
fn cross_join_empty_side() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE a (id INTEGER PRIMARY KEY, val TEXT)");
    exec(&engine, "INSERT INTO a (id, val) VALUES (1, 'x')");
    exec(
        &engine,
        "CREATE TABLE b (id INTEGER PRIMARY KEY, label TEXT)",
    );
    let result = query(&engine, "SELECT * FROM a CROSS JOIN b");
    assert_eq!(result.rows.len(), 0);
}

#[test]
fn right_join_preserves_right() {
    let engine = engine_with_orders();
    exec(
        &engine,
        "INSERT INTO orders (oid, user_id, product) VALUES (13, 99, 'Ghost')",
    );
    let result = query(
        &engine,
        "SELECT users.name, orders.product \
         FROM users RIGHT JOIN orders ON users.id = orders.user_id",
    );
    assert!(str_set(&result, "product").contains("Ghost"));
    assert_eq!(result.rows.len(), 4);
}

#[test]
fn right_join_null_for_unmatched_left() {
    let engine = engine_with_orders();
    exec(
        &engine,
        "INSERT INTO orders (oid, user_id, product) VALUES (13, 99, 'Ghost')",
    );
    let result = query(
        &engine,
        "SELECT users.name, orders.product \
         FROM users RIGHT JOIN orders ON users.id = orders.user_id",
    );
    let ghost: Vec<_> = result
        .rows
        .iter()
        .filter(|r| r.get("product") == Some(&Value::Str("Ghost".into())))
        .collect();
    assert_eq!(ghost.len(), 1);
    assert_eq!(ghost[0].get("name"), Some(&Value::Null));
}

#[test]
fn full_join_preserves_both() {
    let engine = engine_with_orders();
    exec(
        &engine,
        "INSERT INTO orders (oid, user_id, product) VALUES (13, 99, 'Ghost')",
    );
    let result = query(
        &engine,
        "SELECT users.name, orders.product \
         FROM users FULL OUTER JOIN orders ON users.id = orders.user_id",
    );
    assert_eq!(result.rows.len(), 5);
    assert!(str_set(&result, "name").contains("Carol"));
    assert!(str_set(&result, "product").contains("Ghost"));
}

#[test]
fn full_join_no_overlap() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE a (id INTEGER PRIMARY KEY, val TEXT)");
    exec(&engine, "INSERT INTO a (id, val) VALUES (1, 'x')");
    exec(&engine, "CREATE TABLE b (id INTEGER PRIMARY KEY, val TEXT)");
    exec(&engine, "INSERT INTO b (id, val) VALUES (2, 'y')");
    let result = query(&engine, "SELECT * FROM a FULL OUTER JOIN b ON a.id = b.id");
    assert_eq!(result.rows.len(), 2);
}

#[test]
fn ordinary_join_star_uses_public_labels_and_preserves_repeated_values() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE star_left (id INTEGER, value TEXT)");
    exec(&engine, "CREATE TABLE star_right (id INTEGER, value TEXT)");
    exec(&engine, "INSERT INTO star_left VALUES (1, 'left')");
    exec(&engine, "INSERT INTO star_right VALUES (1, 'right')");
    let result = query(
        &engine,
        "SELECT * FROM star_left l JOIN star_right r ON l.id = r.id",
    );
    assert_eq!(result.columns, ["id", "value", "id", "value"]);
    assert_eq!(result.value_at(0, 0), Some(&Value::Int(1)));
    assert_eq!(result.value_at(0, 1), Some(&Value::Str("left".into())));
    assert_eq!(result.value_at(0, 2), Some(&Value::Int(1)));
    assert_eq!(result.value_at(0, 3), Some(&Value::Str("right".into())));
}

#[test]
fn join_using_merges_the_key_and_retains_input_qualification() {
    let engine = using_engine();
    let result = query(
        &engine,
        "SELECT id, l.id AS left_id, r.id AS right_id, l.l_only, r.r_only
         FROM join_left l JOIN join_right r USING (id)",
    );
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0]["id"], Value::Int(1));
    assert_eq!(result.rows[0]["left_id"], Value::Int(1));
    assert_eq!(result.rows[0]["right_id"], Value::Int(1));
    assert_eq!(result.rows[0]["l_only"], Value::Str("l1".into()));
    assert_eq!(result.rows[0]["r_only"], Value::Str("r1".into()));
}

#[test]
fn join_using_keeps_binding_columns_when_projection_does_not_reference_them() {
    let engine = using_engine();
    let result = query(
        &engine,
        "SELECT l.l_only FROM join_left l JOIN join_right r USING (id)",
    );
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0]["l_only"], Value::Str("l1".into()));
}

#[test]
fn nested_join_using_rebinds_the_merged_row_type() {
    let engine = using_engine();
    exec(&engine, "CREATE TABLE join_third (id INTEGER, t_only TEXT)");
    exec(
        &engine,
        "INSERT INTO join_third VALUES (1, 't1'), (4, 't4')",
    );
    let result = query(
        &engine,
        "SELECT id, l.id AS left_id, r.id AS right_id, t.id AS third_id, t.t_only
         FROM join_left l
         JOIN join_right r USING (id)
         JOIN join_third t USING (id)",
    );
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0]["id"], Value::Int(1));
    assert_eq!(result.rows[0]["left_id"], Value::Int(1));
    assert_eq!(result.rows[0]["right_id"], Value::Int(1));
    assert_eq!(result.rows[0]["third_id"], Value::Int(1));
    assert_eq!(result.rows[0]["t_only"], Value::Str("t1".into()));
}

#[test]
fn join_using_outer_variants_choose_the_postgresql_merged_value() {
    let engine = using_engine();
    for (join, expected) in [
        ("LEFT JOIN", vec![Value::Int(1), Value::Int(2), Value::Null]),
        (
            "RIGHT JOIN",
            vec![Value::Int(1), Value::Int(3), Value::Null],
        ),
        (
            "FULL JOIN",
            vec![
                Value::Int(1),
                Value::Int(2),
                Value::Int(3),
                Value::Null,
                Value::Null,
            ],
        ),
    ] {
        let result = query(
            &engine,
            &format!(
                "SELECT id FROM join_left l {join} join_right r USING (id) ORDER BY id NULLS LAST"
            ),
        );
        assert_eq!(
            result
                .rows
                .iter()
                .map(|row| row["id"].clone())
                .collect::<Vec<_>>(),
            expected,
            "{join}"
        );
    }
}

#[test]
fn natural_join_derives_common_columns_in_left_order() {
    let engine = using_engine();
    let result = query(
        &engine,
        "SELECT * FROM join_left l NATURAL JOIN join_right r",
    );
    assert_eq!(result.columns, ["id", "shared", "l_only", "r_only"]);
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0]["id"], Value::Int(1));
    assert_eq!(result.rows[0]["shared"], Value::Str("same".into()));
    assert_eq!(result.rows[0]["l_only"], Value::Str("l1".into()));
    assert_eq!(result.rows[0]["r_only"], Value::Str("r1".into()));
}

#[test]
fn natural_join_keeps_all_binding_columns_when_projection_omits_them() {
    let engine = using_engine();
    let result = query(
        &engine,
        "SELECT l.l_only FROM join_left l NATURAL JOIN join_right r",
    );
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0]["l_only"], Value::Str("l1".into()));
}

#[test]
fn natural_join_binds_a_materialized_cte_row_type() {
    let engine = using_engine();
    let result = query(
        &engine,
        "WITH left_cte AS (SELECT id, shared, l_only FROM join_left)
         SELECT l.l_only
         FROM left_cte l NATURAL JOIN join_right r",
    );
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0]["l_only"], Value::Str("l1".into()));
}

#[test]
fn natural_join_without_common_columns_is_cartesian() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE natural_left (a INTEGER)");
    exec(&engine, "CREATE TABLE natural_right (b TEXT)");
    exec(&engine, "INSERT INTO natural_left VALUES (1), (2)");
    exec(&engine, "INSERT INTO natural_right VALUES ('x'), ('y')");
    let result = query(
        &engine,
        "SELECT * FROM natural_left NATURAL JOIN natural_right",
    );
    assert_eq!(result.columns, ["a", "b"]);
    assert_eq!(result.rows.len(), 4);
}

#[test]
fn join_using_alias_names_only_the_merged_columns() {
    let engine = using_engine();
    let result = query(
        &engine,
        "SELECT joined.id, l.id AS left_id, r.id AS right_id
         FROM join_left l FULL JOIN join_right r USING (id) AS joined
         WHERE joined.id = 3",
    );
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0]["id"], Value::Int(3));
    assert_eq!(result.rows[0]["left_id"], Value::Null);
    assert_eq!(result.rows[0]["right_id"], Value::Int(3));
}

#[test]
fn join_using_and_natural_bind_a_lateral_row_type() {
    let engine = using_engine();
    for qualification in ["USING (id)", "NATURAL JOIN"] {
        let sql = if qualification == "NATURAL JOIN" {
            "SELECT id, l.id AS left_id, r.id AS right_id, r.note
             FROM join_left l NATURAL JOIN LATERAL
                  (SELECT l.id AS id, 'seen' AS note) r
             ORDER BY id"
                .to_string()
        } else {
            format!(
                "SELECT id, l.id AS left_id, r.id AS right_id, r.note
                 FROM join_left l JOIN LATERAL
                      (SELECT l.id AS id, 'seen' AS note) r {qualification}
                 ORDER BY id"
            )
        };
        let result = query(&engine, &sql);
        assert_eq!(result.rows.len(), 2, "{qualification}");
        assert_eq!(result.rows[0]["id"], Value::Int(1));
        assert_eq!(result.rows[1]["id"], Value::Int(2));
        assert_eq!(result.rows[0]["left_id"], Value::Int(1));
        assert_eq!(result.rows[0]["right_id"], Value::Int(1));
        assert_eq!(result.rows[0]["note"], Value::Str("seen".into()));
    }
}

#[test]
fn join_using_reports_postgresql_column_errors() {
    let engine = using_engine();
    let missing = engine
        .sql(
            "SELECT * FROM join_left JOIN join_right USING (missing)",
            &[],
        )
        .unwrap_err();
    assert_eq!(missing.sqlstate(), Some("42703"));
    assert!(missing.to_string().contains("does not exist in left table"));

    let duplicate = engine
        .sql(
            "SELECT * FROM join_left JOIN join_right USING (id, id)",
            &[],
        )
        .unwrap_err();
    assert_eq!(duplicate.sqlstate(), Some("42701"));
}

#[test]
fn join_using_resolves_postgresql_common_types_before_execution() {
    let engine = Engine::new();
    let result = query(
        &engine,
        "SELECT pg_typeof(id) AS ty, id
         FROM (VALUES (1::smallint)) AS l(id)
         FULL JOIN (VALUES (1::bigint)) AS r(id) USING (id)",
    );
    assert_eq!(result.rows[0]["ty"], Value::Str("bigint".into()));
    assert_eq!(result.rows[0]["id"], Value::Int(1));

    let varchar_left = query(
        &engine,
        "SELECT pg_typeof(id) AS ty
         FROM (VALUES ('x'::varchar)) AS l(id)
         FULL JOIN (VALUES ('x'::text)) AS r(id) USING (id)",
    );
    assert_eq!(
        varchar_left.rows[0]["ty"],
        Value::Str("character varying".into())
    );

    let text_left = query(
        &engine,
        "SELECT pg_typeof(id) AS ty
         FROM (VALUES ('x'::text)) AS l(id)
         FULL JOIN (VALUES ('x'::varchar)) AS r(id) USING (id)",
    );
    assert_eq!(text_left.rows[0]["ty"], Value::Str("text".into()));
}

#[test]
fn join_using_preserves_types_through_tables_subqueries_and_cte_spill() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE typed_left (id SMALLINT)");
    exec(&engine, "CREATE TABLE typed_right (id BIGINT)");
    exec(&engine, "INSERT INTO typed_left VALUES (1)");
    exec(&engine, "INSERT INTO typed_right VALUES (1)");

    for sql in [
        "SELECT pg_typeof(id) AS ty FROM typed_left FULL JOIN typed_right USING (id)",
        "SELECT pg_typeof(id) AS ty
         FROM (SELECT id FROM typed_left) l
         FULL JOIN (SELECT id FROM typed_right) r USING (id)",
        "WITH l AS (SELECT id FROM typed_left), r AS (SELECT id FROM typed_right)
         SELECT pg_typeof(id) AS ty FROM l FULL JOIN r USING (id)",
    ] {
        let result = query(&engine, sql);
        assert_eq!(result.rows[0]["ty"], Value::Str("bigint".into()), "{sql}");
    }
}

#[test]
fn join_using_resolves_static_table_function_types() {
    let engine = Engine::new();
    let result = query(
        &engine,
        "SELECT pg_typeof(id) AS ty, id
         FROM (VALUES (1::smallint)) AS l(id)
         JOIN generate_series(1::bigint, 1::bigint) AS r(id) USING (id)",
    );
    assert_eq!(result.rows[0]["ty"], Value::Str("bigint".into()));
    assert_eq!(result.rows[0]["id"], Value::Int(1));

    let json = query(
        &engine,
        "SELECT pg_typeof(key) AS key_type, pg_typeof(value) AS value_type
         FROM json_each('{\"a\": 1}'::json)",
    );
    assert_eq!(json.rows[0]["key_type"], Value::Str("text".into()));
    assert_eq!(json.rows[0]["value_type"], Value::Str("json".into()));
}

#[test]
fn join_using_rejects_undefined_postgresql_equality_operators() {
    let engine = Engine::new();
    for sql in [
        "SELECT * FROM (VALUES (true)) l(id) JOIN (VALUES (1)) r(id) USING (id)",
        "SELECT * FROM (VALUES ('{}'::json)) l(id) JOIN (VALUES ('{}'::json)) r(id) USING (id)",
        "SELECT * FROM (VALUES (ARRAY[1]::integer[])) l(id)
         JOIN (VALUES (ARRAY[1]::bigint[])) r(id) USING (id)",
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42883"), "{sql}: {error}");
    }
}

#[test]
fn ordinary_join_rejects_an_ambiguous_unqualified_column() {
    let engine = using_engine();
    let error = engine
        .sql("SELECT id FROM join_left l JOIN join_right r ON true", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42702"));
}

#[test]
fn cursor_preserves_different_duplicate_non_using_columns() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE cursor_left (id INTEGER, shared TEXT)",
    );
    exec(
        &engine,
        "CREATE TABLE cursor_right (id INTEGER, shared TEXT)",
    );
    exec(&engine, "INSERT INTO cursor_left VALUES (1, 'left-value')");
    exec(
        &engine,
        "INSERT INTO cursor_right VALUES (1, 'right-value')",
    );
    let result = query(
        &engine,
        "SELECT * FROM cursor_left l JOIN cursor_right r USING (id)",
    );
    assert_eq!(result.columns, ["id", "shared", "shared"]);
    assert_eq!(result.value_at(0, 0), Some(&Value::Int(1)));
    assert_eq!(
        result.value_at(0, 1),
        Some(&Value::Str("left-value".into()))
    );
    assert_eq!(
        result.value_at(0, 2),
        Some(&Value::Str("right-value".into()))
    );
    let mut cursor = engine
        .sql_cursor(
            "SELECT * FROM cursor_left l JOIN cursor_right r USING (id)",
            &[],
        )
        .unwrap();
    assert_eq!(cursor.columns(), ["id", "shared", "shared"]);
    let batch = cursor.next().unwrap().unwrap();
    assert_eq!(batch.columns()[0].values, [Value::Int(1)]);
    assert_eq!(batch.columns()[1].values, [Value::Str("left-value".into())]);
    assert_eq!(
        batch.columns()[2].values,
        [Value::Str("right-value".into())]
    );
}

#[test]
fn implicit_cross_join() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE a (id INTEGER PRIMARY KEY, val TEXT)");
    exec(&engine, "INSERT INTO a (id, val) VALUES (1, 'x')");
    exec(&engine, "INSERT INTO a (id, val) VALUES (2, 'y')");
    exec(
        &engine,
        "CREATE TABLE b (id INTEGER PRIMARY KEY, label TEXT)",
    );
    exec(&engine, "INSERT INTO b (id, label) VALUES (10, 'p')");
    exec(&engine, "INSERT INTO b (id, label) VALUES (20, 'q')");
    let result = query(&engine, "SELECT a.val, b.label FROM a, b");
    assert_eq!(result.rows.len(), 4);
}

#[test]
fn implicit_cross_join_with_where() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)",
    );
    exec(&engine, "INSERT INTO users (id, name) VALUES (1, 'Alice')");
    exec(&engine, "INSERT INTO users (id, name) VALUES (2, 'Bob')");
    exec(
        &engine,
        "CREATE TABLE orders (oid INTEGER PRIMARY KEY, user_id INTEGER, product TEXT)",
    );
    exec(
        &engine,
        "INSERT INTO orders (oid, user_id, product) VALUES (10, 1, 'Book')",
    );
    exec(
        &engine,
        "INSERT INTO orders (oid, user_id, product) VALUES (11, 2, 'Pen')",
    );
    let result = query(
        &engine,
        "SELECT users.name, orders.product \
         FROM users, orders WHERE users.id = orders.user_id",
    );
    assert_eq!(result.rows.len(), 2);
}

#[test]
fn three_table_cross_join() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE a (id INTEGER PRIMARY KEY, x TEXT)");
    exec(&engine, "INSERT INTO a (id, x) VALUES (1, 'a')");
    exec(&engine, "CREATE TABLE b (id INTEGER PRIMARY KEY, y TEXT)");
    exec(&engine, "INSERT INTO b (id, y) VALUES (1, 'b')");
    exec(&engine, "CREATE TABLE c (id INTEGER PRIMARY KEY, z TEXT)");
    exec(&engine, "INSERT INTO c (id, z) VALUES (1, 'c')");
    exec(&engine, "INSERT INTO c (id, z) VALUES (2, 'd')");
    let result = query(&engine, "SELECT a.x, b.y, c.z FROM a, b, c");
    assert_eq!(result.rows.len(), 2);
}

#[test]
fn lateral_subquery_with_aggregate() {
    let engine = lateral_engine();
    let result = query(
        &engine,
        "SELECT d.dept_name, sub.top_salary \
         FROM depts d, \
         LATERAL (SELECT MAX(salary) AS top_salary \
         FROM emps WHERE emps.dept_id = d.id) sub \
         ORDER BY d.dept_name",
    );
    assert_eq!(result.rows.len(), 2);
    assert_eq!(
        result.rows[0]["dept_name"],
        Value::Str("Engineering".into())
    );
    assert_eq!(result.rows[0]["top_salary"], Value::Int(90000));
    assert_eq!(result.rows[1]["dept_name"], Value::Str("Sales".into()));
    assert_eq!(result.rows[1]["top_salary"], Value::Int(75000));
}

#[test]
fn lateral_with_limit() {
    let engine = lateral_engine();
    let result = query(
        &engine,
        "SELECT d.dept_name, sub.top_emp, sub.top_sal \
         FROM depts d, \
         LATERAL (SELECT emp_name AS top_emp, salary AS top_sal \
         FROM emps WHERE emps.dept_id = d.id \
         ORDER BY salary DESC LIMIT 1) sub \
         ORDER BY d.dept_name",
    );
    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.rows[0]["top_emp"], Value::Str("Alice".into()));
    assert_eq!(result.rows[0]["top_sal"], Value::Int(90000));
    assert_eq!(result.rows[1]["top_emp"], Value::Str("Diana".into()));
    assert_eq!(result.rows[1]["top_sal"], Value::Int(75000));
}

#[test]
fn lateral_with_count() {
    let engine = lateral_engine();
    let result = query(
        &engine,
        "SELECT d.dept_name, sub.emp_count \
         FROM depts d, \
         LATERAL (SELECT COUNT(*) AS emp_count \
         FROM emps WHERE emps.dept_id = d.id) sub \
         ORDER BY d.dept_name",
    );
    assert_eq!(
        result.rows[0]["dept_name"],
        Value::Str("Engineering".into())
    );
    assert_eq!(result.rows[0]["emp_count"], Value::Int(2));
    assert_eq!(result.rows[1]["dept_name"], Value::Str("Sales".into()));
    assert_eq!(result.rows[1]["emp_count"], Value::Int(2));
}

#[test]
fn lateral_subqueries_preserve_outer_and_output_type_identity() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE lateral_types (v SMALLINT, label VARCHAR(7), score REAL)",
    );
    exec(
        &engine,
        "INSERT INTO lateral_types VALUES (7, 'seven', 1.5)",
    );

    let outer_types = query(
        &engine,
        "SELECT s.v_type, s.label_type, s.score_type
         FROM lateral_types AS l
         CROSS JOIN LATERAL (
             SELECT pg_typeof(l.v) AS v_type,
                    pg_typeof(l.label) AS label_type,
                    pg_typeof(l.score) AS score_type
         ) AS s",
    );
    assert_eq!(outer_types.rows[0]["v_type"], Value::Str("smallint".into()));
    assert_eq!(
        outer_types.rows[0]["label_type"],
        Value::Str("character varying".into())
    );
    assert_eq!(outer_types.rows[0]["score_type"], Value::Str("real".into()));

    let output_types = query(
        &engine,
        "SELECT s.v, s.label, s.score
         FROM lateral_types AS l
         CROSS JOIN LATERAL (
             SELECT l.v::bigint AS v,
                    l.label::varchar(3) AS label,
                    l.score::double precision AS score
         ) AS s",
    );
    assert_eq!(
        output_types.column_types,
        [
            Some(uqa_sql::ColumnType::BigInteger),
            Some(uqa_sql::ColumnType::Varchar(Some(3))),
            Some(uqa_sql::ColumnType::DoublePrecision),
        ]
    );
}

#[test]
fn empty_cte_lateral_source_keeps_its_declared_type() {
    let engine = Engine::new();
    let result = query(
        &engine,
        "WITH c AS (SELECT 1::smallint AS v WHERE false)
         SELECT pg_typeof(s.v) AS ty, s.v
         FROM (VALUES (1)) AS seed(n)
         LEFT JOIN LATERAL (SELECT v FROM c) AS s ON true",
    );
    assert_eq!(result.rows[0]["ty"], Value::Str("smallint".into()));
    assert_eq!(result.rows[0]["v"], Value::Null);
    assert_eq!(
        result.column_types,
        [
            Some(uqa_sql::ColumnType::Regtype),
            Some(uqa_sql::ColumnType::SmallInteger),
        ]
    );
}
