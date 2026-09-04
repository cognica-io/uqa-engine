//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn table_range_alias_renames_columns_positionally() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE table_alias_source (id INTEGER, label TEXT)",
    );
    exec(
        &engine,
        "INSERT INTO table_alias_source VALUES (2, 'two'), (1, 'one')",
    );
    let result = query(
        &engine,
        "SELECT source.key, source.value FROM table_alias_source AS source(key, value) WHERE source.key > 1 ORDER BY source.key",
    );
    assert_eq!(result.columns, ["key", "value"]);
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0]["key"], Value::Int(2));
    assert_eq!(result.rows[0]["value"], Value::Str("two".into()));

    let partial = query(
        &engine,
        "SELECT source.* FROM table_alias_source AS source(key) WHERE source.key = 1",
    );
    assert_eq!(partial.columns, ["key", "label"]);
    assert_eq!(partial.rows[0]["key"], Value::Int(1));
    assert_eq!(partial.rows[0]["label"], Value::Str("one".into()));
    let hidden = engine
        .sql(
            "SELECT source.id FROM table_alias_source AS source(key)",
            &[],
        )
        .unwrap_err();
    assert_eq!(hidden.sqlstate(), Some("42703"));

    let error = engine
        .sql("SELECT * FROM table_alias_source AS source(a, b, c)", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42P10"));
    assert_eq!(
        error.to_string(),
        "table \"source\" has 2 columns available but 3 columns specified"
    );
}

#[test]
fn table_range_aliases_apply_to_views_ctes_and_sequences() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE range_alias_base(id INTEGER, label TEXT);
         INSERT INTO range_alias_base VALUES (1, 'one');
         CREATE VIEW range_alias_view AS SELECT id, label FROM range_alias_base;
         CREATE SEQUENCE range_alias_sequence START WITH 5",
    );
    for sql in [
        "SELECT source.key, source.value FROM range_alias_view AS source(key, value)",
        "WITH items AS MATERIALIZED (SELECT id, label FROM range_alias_base) SELECT source.key, source.value FROM items AS source(key, value)",
        "WITH items AS NOT MATERIALIZED (SELECT id, label FROM range_alias_base) SELECT source.key, source.value FROM items AS source(key, value)",
    ] {
        let result = query(&engine, sql);
        assert_eq!(result.columns, ["key", "value"], "{sql}");
        assert_eq!(result.rows[0]["key"], Value::Int(1), "{sql}");
        assert_eq!(result.rows[0]["value"], Value::Str("one".into()), "{sql}");
    }
    let sequence = query(
        &engine,
        "SELECT source.current_value, source.called FROM range_alias_sequence AS source(current_value, log_count, called)",
    );
    assert_eq!(sequence.rows[0]["current_value"], Value::Int(5));
    assert_eq!(sequence.rows[0]["called"], Value::Bool(false));
}

#[test]
fn table_range_aliases_hide_physical_names_during_correlation_analysis() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE alias_correlation_outer(id INTEGER);
         CREATE TABLE alias_correlation_inner(id INTEGER);
         INSERT INTO alias_correlation_outer VALUES (1), (2);
         INSERT INTO alias_correlation_inner VALUES (100)",
    );
    let result = query(
        &engine,
        "SELECT outer_source.id
         FROM alias_correlation_outer AS outer_source
         WHERE EXISTS (
           SELECT 1
           FROM alias_correlation_inner AS inner_source(inner_id)
           WHERE id = 2
         )
         ORDER BY outer_source.id",
    );
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0]["id"], Value::Int(2));
}

#[test]
fn natural_join_uses_table_range_aliases_for_binding_and_pruning() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE range_alias_left(left_id INTEGER, left_value TEXT);
         CREATE TABLE range_alias_right(right_id INTEGER, right_value TEXT);
         INSERT INTO range_alias_left VALUES (1, 'left'), (2, 'left-2');
         INSERT INTO range_alias_right VALUES (1, 'right'), (3, 'right-3')",
    );
    let result = query(
        &engine,
        "SELECT join_key, left_label, right_label
         FROM range_alias_left AS left_source(join_key, left_label)
         NATURAL JOIN range_alias_right AS right_source(join_key, right_label)
         WHERE join_key = 1",
    );
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0]["join_key"], Value::Int(1));
    assert_eq!(result.rows[0]["left_label"], Value::Str("left".into()));
    assert_eq!(result.rows[0]["right_label"], Value::Str("right".into()));
}

#[test]
fn retrieval_fields_follow_table_range_aliases_to_physical_indexes() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE range_alias_search(search_id INTEGER, body TEXT);
         INSERT INTO range_alias_search VALUES (1, 'rust database'), (2, 'other text');
         CREATE INDEX range_alias_search_body ON range_alias_search USING gin (body)",
    );
    let result = query(
        &engine,
        "SELECT document_key
         FROM range_alias_search AS documents(document_key, content)
         WHERE text_match(documents.content, 'rust')",
    );
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0]["document_key"], Value::Int(1));
}

#[test]
fn qualified_star_projects_only_the_named_relation() {
    let engine = engine_with_orders();
    let result = query(
        &engine,
        "SELECT u.*, o.product
         FROM users AS u JOIN orders AS o ON o.user_id = u.id
         WHERE o.oid = 10",
    );
    assert_eq!(result.columns, ["id", "name", "product"]);
    assert_eq!(result.rows[0]["id"], Value::Int(1));
    assert_eq!(result.rows[0]["name"], Value::Str("Alice".into()));
    assert_eq!(result.rows[0]["product"], Value::Str("Book".into()));
}

#[test]
fn qualified_star_preserves_using_side_values() {
    let engine = using_engine();
    let result = query(
        &engine,
        "SELECT r.*
         FROM join_left AS l LEFT JOIN join_right AS r USING (id)
         WHERE l.id IN (1, 2)
         ORDER BY l.id",
    );
    assert_eq!(result.columns, ["id", "shared", "r_only"]);
    assert_eq!(result.rows[0]["id"], Value::Int(1));
    assert_eq!(result.rows[0]["shared"], Value::Str("same".into()));
    assert_eq!(result.rows[1]["id"], Value::Null);
    assert_eq!(result.rows[1]["shared"], Value::Null);
    assert_eq!(result.rows[1]["r_only"], Value::Null);
}

#[test]
fn qualified_star_keeps_dots_inside_quoted_identifiers() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE dotted_star (\"c.d\" INTEGER, plain TEXT)",
    );
    exec(&engine, "INSERT INTO dotted_star VALUES (7, 'seven')");
    let result = query(
        &engine,
        "SELECT \"a.b\".* FROM dotted_star AS \"a.b\" ORDER BY \"a.b\".\"c.d\"",
    );
    assert_eq!(result.columns, ["c.d", "plain"]);
    assert_eq!(result.rows[0]["c.d"], Value::Int(7));
    assert_eq!(result.rows[0]["plain"], Value::Str("seven".into()));
}

#[test]
fn qualified_star_rejects_an_unknown_relation() {
    let engine = engine_with_orders();
    let error = engine
        .sql("SELECT missing.* FROM users AS u", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42P01"));
}

#[test]
fn parenthesized_join_alias_renames_the_complete_join_output() {
    let engine = Engine::new();
    let result = query(
        &engine,
        "SELECT j.joined_id, j.left_value, j.right_value
         FROM ((VALUES (1, 'left'), (2, 'left-2')) AS l(id, left_value)
         FULL JOIN (VALUES (1, 'right'), (3, 'right-3')) AS r(id, right_value)
         USING (id)) AS j(joined_id)
         ORDER BY j.joined_id",
    );
    assert_eq!(result.columns, ["joined_id", "left_value", "right_value"]);
    assert_eq!(result.rows.len(), 3);
    assert_eq!(result.rows[0]["joined_id"], Value::Int(1));
    assert_eq!(result.rows[0]["left_value"], Value::Str("left".into()));
    assert_eq!(result.rows[0]["right_value"], Value::Str("right".into()));
    assert_eq!(result.rows[1]["joined_id"], Value::Int(2));
    assert_eq!(result.rows[1]["right_value"], Value::Null);
    assert_eq!(result.rows[2]["joined_id"], Value::Int(3));
    assert_eq!(result.rows[2]["left_value"], Value::Null);

    let filtered = query(
        &engine,
        "SELECT j.joined_id
         FROM ((VALUES (1), (2)) AS l(id) JOIN (VALUES (1), (2)) AS r(id) USING (id)) AS j(joined_id)
         WHERE j.joined_id = 2",
    );
    assert_eq!(filtered.rows.len(), 1);
    assert_eq!(filtered.rows[0]["joined_id"], Value::Int(2));
}

#[test]
fn parenthesized_join_alias_is_visible_to_outer_joins_and_lateral_sources() {
    let engine = Engine::new();
    let result = query(
        &engine,
        "SELECT j.id, x.label, q.next_id
         FROM ((VALUES (1)) AS l(id) JOIN (VALUES (1)) AS r(id) USING (id)) AS j
         JOIN (VALUES (1, 'matched')) AS x(id, label) ON j.id = x.id
         CROSS JOIN LATERAL (SELECT j.id + 1 AS next_id) AS q",
    );
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0]["id"], Value::Int(1));
    assert_eq!(result.rows[0]["label"], Value::Str("matched".into()));
    assert_eq!(result.rows[0]["next_id"], Value::Int(2));
}

#[test]
fn parenthesized_join_alias_hides_input_and_using_aliases() {
    let engine = Engine::new();
    for sql in [
        "SELECT l.id FROM ((VALUES (1)) AS l(id) JOIN (VALUES (1)) AS r(id) USING (id)) AS j",
        "SELECT merged.id FROM ((VALUES (1)) AS l(id) JOIN (VALUES (1)) AS r(id) USING (id) AS merged) AS j",
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42P01"), "{sql}: {error}");
    }

    let own_on = engine
        .sql(
            "SELECT j.* FROM ((VALUES (1)) AS l(id) JOIN (VALUES (1)) AS r(id) ON j.id = r.id) AS j",
            &[],
        )
        .unwrap_err();
    assert_eq!(own_on.sqlstate(), Some("42P01"));
}

#[test]
fn parenthesized_join_alias_validates_column_names_like_postgresql() {
    let engine = Engine::new();
    let too_many = engine
        .sql(
            "SELECT j.* FROM ((VALUES (1)) AS l(id) JOIN (VALUES (1)) AS r(id) USING (id)) AS j(a, b)",
            &[],
        )
        .unwrap_err();
    assert_eq!(too_many.sqlstate(), Some("42P10"));
    assert_eq!(
        too_many.to_string(),
        "join expression \"j\" has 1 columns available but 2 columns specified"
    );

    for sql in [
        "SELECT j.id FROM ((VALUES (1)) AS l(id) JOIN (VALUES (1)) AS r(id) ON l.id = r.id) AS j",
        "SELECT j.a FROM ((VALUES (1)) AS l(id) JOIN (VALUES (1)) AS r(id) ON l.id = r.id) AS j(a, a)",
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42702"), "{sql}: {error}");
    }
}

#[test]
fn row_locking_distinguishes_a_join_alias_from_its_inputs() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE lock_left (id INTEGER PRIMARY KEY)");
    exec(&engine, "CREATE TABLE lock_right (id INTEGER PRIMARY KEY)");
    exec(&engine, "INSERT INTO lock_left VALUES (1)");
    exec(&engine, "INSERT INTO lock_right VALUES (1)");

    let result = query(
        &engine,
        "SELECT j.* FROM (lock_left AS l JOIN lock_right AS r USING (id)) AS j FOR UPDATE OF l",
    );
    assert_eq!(result.rows.len(), 1);

    let reduced = query(
        &engine,
        "SELECT j.*
         FROM (lock_left AS l LEFT JOIN lock_right AS r ON l.id = r.id) AS j(left_id, right_id)
         WHERE j.right_id IS NOT NULL
         FOR UPDATE",
    );
    assert_eq!(reduced.rows.len(), 1);

    let nullable = engine
        .sql(
            "SELECT j.*
             FROM (lock_left AS l LEFT JOIN lock_right AS r ON l.id = r.id) AS j(left_id, right_id)
             FOR UPDATE",
            &[],
        )
        .unwrap_err();
    assert_eq!(nullable.sqlstate(), Some("0A000"));

    let error = engine
        .sql(
            "SELECT j.* FROM (lock_left AS l JOIN lock_right AS r USING (id)) AS j FOR UPDATE OF j",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("0A000"));
    assert_eq!(
        error.to_string(),
        "unsupported SQL feature: FOR UPDATE cannot be applied to a join"
    );
}
