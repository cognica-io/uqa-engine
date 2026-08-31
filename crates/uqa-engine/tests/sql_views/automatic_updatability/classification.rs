//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

pub(super) fn assert_check_option_definition_over_non_updatable_source() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE check_source_base (id INTEGER PRIMARY KEY);
         CREATE VIEW check_source_inner AS SELECT DISTINCT id FROM check_source_base;
         CREATE VIEW check_source_outer AS
           SELECT id FROM check_source_inner WHERE id > 0 WITH CHECK OPTION",
    );
    let flags = exec(
        &engine,
        "SELECT is_updatable, is_insertable_into
         FROM information_schema.views
         WHERE table_name = 'check_source_outer'",
    );
    assert_eq!(flags.rows[0]["is_updatable"], Value::Str("NO".into()));
    assert_eq!(flags.rows[0]["is_insertable_into"], Value::Str("NO".into()));
}

pub(super) fn assert_rule_catalog_flags_ignore_replication_mode() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE mode_catalog_base (id INTEGER);
         CREATE VIEW mode_catalog_view AS SELECT DISTINCT id FROM mode_catalog_base;
         CREATE RULE mode_catalog_insert AS ON INSERT TO mode_catalog_view DO INSTEAD NOTHING;
         SET session_replication_role = replica",
    );
    let flags = exec(
        &engine,
        "SELECT is_insertable_into FROM information_schema.views
         WHERE table_name = 'mode_catalog_view'",
    );
    assert_eq!(
        flags.rows[0]["is_insertable_into"],
        Value::Str("YES".into())
    );
    exec(&engine, "RESET session_replication_role");
}

pub(super) fn assert_select_star_without_a_relation_is_rejected() {
    let engine = Engine::new();
    let select = engine.sql("SELECT *", &[]).unwrap_err();
    assert_eq!(select.sqlstate(), Some("42601"));
    let view = engine
        .sql("CREATE VIEW invalid_star_view AS SELECT *", &[])
        .unwrap_err();
    assert_eq!(view.sqlstate(), Some("42601"));
}

pub(super) fn assert_rule_updatability_catalog_flags() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE catalog_rule_base (id INTEGER);
         CREATE VIEW catalog_rule_insert AS SELECT DISTINCT id FROM catalog_rule_base;
         CREATE RULE catalog_rule_insert_rule AS ON INSERT TO catalog_rule_insert DO INSTEAD NOTHING;
         CREATE VIEW catalog_rule_update AS SELECT DISTINCT id FROM catalog_rule_base;
         CREATE RULE catalog_rule_update_rule AS ON UPDATE TO catalog_rule_update DO INSTEAD NOTHING;
         CREATE VIEW catalog_rule_all AS SELECT DISTINCT id FROM catalog_rule_base;
         CREATE RULE catalog_rule_all_insert AS ON INSERT TO catalog_rule_all DO INSTEAD NOTHING;
         CREATE RULE catalog_rule_all_update AS ON UPDATE TO catalog_rule_all DO INSTEAD NOTHING;
         CREATE RULE catalog_rule_all_delete AS ON DELETE TO catalog_rule_all DO INSTEAD NOTHING",
    );
    let views = exec(
        &engine,
        "SELECT table_name, is_updatable, is_insertable_into
         FROM information_schema.views
         WHERE table_name IN ('catalog_rule_insert', 'catalog_rule_update', 'catalog_rule_all')
         ORDER BY table_name",
    );
    assert_eq!(
        views.rows[0]["table_name"],
        Value::Str("catalog_rule_all".into())
    );
    assert_eq!(views.rows[0]["is_updatable"], Value::Str("YES".into()));
    assert_eq!(
        views.rows[0]["is_insertable_into"],
        Value::Str("YES".into())
    );
    assert_eq!(views.rows[1]["is_updatable"], Value::Str("NO".into()));
    assert_eq!(
        views.rows[1]["is_insertable_into"],
        Value::Str("YES".into())
    );
    assert_eq!(views.rows[2]["is_updatable"], Value::Str("NO".into()));
    assert_eq!(views.rows[2]["is_insertable_into"], Value::Str("NO".into()));
    let columns = exec(
        &engine,
        "SELECT table_name, is_updatable FROM information_schema.columns
         WHERE table_name IN ('catalog_rule_insert', 'catalog_rule_update', 'catalog_rule_all')
         ORDER BY table_name",
    );
    assert_eq!(columns.rows[0]["is_updatable"], Value::Str("YES".into()));
    assert_eq!(columns.rows[1]["is_updatable"], Value::Str("NO".into()));
    assert_eq!(columns.rows[2]["is_updatable"], Value::Str("NO".into()));
}

pub(super) fn assert_check_option_error_order_and_mapped_duplicates() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE check_order_base (id INTEGER PRIMARY KEY, value INTEGER);
         INSERT INTO check_order_base VALUES (1, 1), (2, 2);
         CREATE VIEW check_order_view AS
           SELECT id, value FROM check_order_base WHERE value > 0 WITH CHECK OPTION",
    );
    let insert = engine
        .sql("INSERT INTO check_order_view VALUES (1, -1)", &[])
        .unwrap_err();
    assert_eq!(insert.sqlstate(), Some("23505"));
    let update = engine
        .sql(
            "UPDATE check_order_view SET id = 1, value = -1 WHERE id = 2",
            &[],
        )
        .unwrap_err();
    assert_eq!(update.sqlstate(), Some("23505"));

    exec(
        &engine,
        "CREATE TABLE duplicate_map_base (id INTEGER PRIMARY KEY, value INTEGER);
         CREATE VIEW duplicate_map_view (first_id, second_id, value) AS
           SELECT id, id, value FROM duplicate_map_base",
    );
    let duplicate = engine
        .sql(
            "INSERT INTO duplicate_map_view(first_id, second_id, value) VALUES (1, 1, 2)",
            &[],
        )
        .unwrap_err();
    assert_eq!(duplicate.sqlstate(), Some("42601"));
    exec(
        &engine,
        "INSERT INTO duplicate_map_view(first_id, value) VALUES (1, 2)
         ON CONFLICT(first_id, second_id) DO NOTHING",
    );
    assert_eq!(
        exec(&engine, "SELECT value FROM duplicate_map_base WHERE id = 1").rows[0]["value"],
        Value::Int(2)
    );
}

pub(super) fn assert_nested_conditional_instead_rule_rejects_statement() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE nested_conditional_base (id INTEGER PRIMARY KEY, value INTEGER NOT NULL);
         INSERT INTO nested_conditional_base VALUES (1, 10), (2, 20);
         CREATE VIEW nested_conditional_inner AS SELECT id, value FROM nested_conditional_base;
         CREATE VIEW nested_conditional_outer AS SELECT id, value FROM nested_conditional_inner;
         CREATE RULE nested_conditional_rule AS ON UPDATE TO nested_conditional_inner
           WHERE OLD.id = 1 DO INSTEAD NOTHING",
    );
    let error = engine
        .sql("UPDATE nested_conditional_outer SET value = value + 1", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("55000"));
    let rows = exec(
        &engine,
        "SELECT id, value FROM nested_conditional_base ORDER BY id",
    );
    assert_eq!(rows.rows[0]["value"], Value::Int(10));
    assert_eq!(rows.rows[1]["value"], Value::Int(20));
}

pub(super) fn assert_conditional_instead_rules_do_not_make_views_updatable() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE conditional_rule_base (id INTEGER PRIMARY KEY, value INTEGER NOT NULL);
         INSERT INTO conditional_rule_base VALUES (1, 10);
         CREATE VIEW conditional_insert_view AS SELECT id, value FROM conditional_rule_base;
         CREATE VIEW conditional_update_view AS SELECT id, value FROM conditional_rule_base;
         CREATE VIEW conditional_delete_view AS SELECT id, value FROM conditional_rule_base;
         CREATE RULE conditional_insert AS ON INSERT TO conditional_insert_view
           WHERE NEW.id > 0 DO INSTEAD NOTHING;
         CREATE RULE conditional_update AS ON UPDATE TO conditional_update_view
           WHERE OLD.id > 0 DO INSTEAD NOTHING;
         CREATE RULE conditional_delete AS ON DELETE TO conditional_delete_view
           WHERE OLD.id > 0 DO INSTEAD NOTHING",
    );
    for sql in [
        "INSERT INTO conditional_insert_view VALUES (2, 20)",
        "UPDATE conditional_update_view SET value = 11 WHERE id = 1",
        "DELETE FROM conditional_delete_view WHERE id = 1",
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("55000"), "{sql}: {error}");
    }
    let rows = exec(
        &engine,
        "SELECT id, value FROM conditional_rule_base ORDER BY id",
    );
    assert_eq!(rows.rows.len(), 1);
    assert_eq!(rows.rows[0]["id"], Value::Int(1));
    assert_eq!(rows.rows[0]["value"], Value::Int(10));
}

pub(super) fn assert_view_star_row_type_is_fixed_at_creation() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE fixed_star_base (id INTEGER PRIMARY KEY, value TEXT NOT NULL);
         CREATE VIEW fixed_star_view AS
           SELECT *, value || ':computed' AS computed FROM fixed_star_base;
         ALTER TABLE fixed_star_base ADD COLUMN added_later INTEGER NOT NULL DEFAULT 7",
    );
    let inserted = exec(
        &engine,
        "INSERT INTO fixed_star_view (id, value) VALUES (1, 'one') RETURNING *",
    );
    assert_eq!(inserted.columns, ["id", "value", "computed"]);
    assert_eq!(
        inserted.rows[0]["computed"],
        Value::Str("one:computed".into())
    );
    let selected = exec(&engine, "SELECT * FROM fixed_star_view");
    assert_eq!(selected.columns, ["id", "value", "computed"]);
    assert_eq!(
        selected.rows[0]["computed"],
        Value::Str("one:computed".into())
    );
    let base = exec(
        &engine,
        "SELECT added_later FROM fixed_star_base WHERE id = 1",
    );
    assert_eq!(base.rows[0]["added_later"], Value::Int(7));
}

fn non_updatable_view_engine() -> Engine {
    let engine = automatic_view_engine();
    exec(
        &engine,
        "CREATE TABLE automatic_source (id INTEGER PRIMARY KEY)",
    );
    exec(
        &engine,
        "CREATE VIEW distinct_view AS SELECT DISTINCT visible FROM automatic_base",
    );
    exec(
        &engine,
        "CREATE VIEW aggregate_view AS SELECT visible, count(*) AS total FROM automatic_base GROUP BY visible",
    );
    exec(
        &engine,
        "CREATE VIEW join_view AS
         SELECT base.id, base.value FROM automatic_base AS base
         JOIN automatic_source AS source ON source.id = base.id",
    );
    exec(
        &engine,
        "CREATE TABLE automatic_readonly_base (id INTEGER PRIMARY KEY)",
    );
    exec(&engine, "INSERT INTO automatic_readonly_base VALUES (1)");
    exec(
        &engine,
        "CREATE VIEW constant_only_view AS
         SELECT id + 1 AS computed_id FROM automatic_readonly_base",
    );
    exec(
        &engine,
        "CREATE VIEW xmin_view AS SELECT xmin FROM automatic_readonly_base",
    );
    exec(
        &engine,
        "CREATE VIEW set_returning_view AS
         SELECT id, generate_series(1, 2) AS generated FROM automatic_readonly_base",
    );
    exec(
        &engine,
        "CREATE MATERIALIZED VIEW automatic_materialized AS
         SELECT id, value FROM automatic_base",
    );
    engine
}

pub(super) fn assert_non_updatable_views_and_catalog_flags() {
    let engine = non_updatable_view_engine();
    assert_non_updatable_view_dml(&engine);
    assert_non_updatable_view_catalog_flags(&engine);
}

fn assert_non_updatable_view_dml(engine: &Engine) {
    let invalid_check_option = engine
        .sql(
            "CREATE VIEW invalid_check_option AS
             SELECT DISTINCT visible FROM automatic_base WITH CHECK OPTION",
            &[],
        )
        .unwrap_err();
    assert_eq!(invalid_check_option.sqlstate(), Some("0A000"));
    exec(
        engine,
        "CREATE VIEW altered_invalid_check_option AS
         SELECT DISTINCT visible FROM automatic_base",
    );
    let altered_check_option = engine
        .sql(
            "ALTER VIEW altered_invalid_check_option SET (check_option = local)",
            &[],
        )
        .unwrap_err();
    assert_eq!(altered_check_option.sqlstate(), Some("0A000"));
    let check_option = exec(
        engine,
        "SELECT check_option FROM information_schema.views
         WHERE table_schema = 'public' AND table_name = 'altered_invalid_check_option'",
    );
    assert_eq!(
        check_option.rows[0]["check_option"],
        Value::Str("NONE".into())
    );
    for sql in [
        "INSERT INTO distinct_view VALUES (true)",
        "UPDATE join_view SET value = 'changed'",
        "DELETE FROM aggregate_view",
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("55000"), "{sql}: {error}");
    }
    for sql in [
        "INSERT INTO constant_only_view VALUES (2)",
        "UPDATE constant_only_view SET computed_id = 3",
        "UPDATE set_returning_view SET id = 2",
        "INSERT INTO xmin_view VALUES ('1')",
        "UPDATE xmin_view SET xmin = '1'",
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("55000"), "{sql}: {error}");
    }
    for sql in [
        "INSERT INTO constant_only_view (no_such_column) VALUES (2)",
        "UPDATE constant_only_view SET no_such_column = 3",
        "INSERT INTO join_view (no_such_column) VALUES (2)",
        "UPDATE join_view SET no_such_column = 3",
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42703"), "{sql}: {error}");
    }
    for sql in [
        "UPDATE join_view AS target SET value = 'changed' WHERE target.secret = 'hidden'",
        "UPDATE join_view SET value = 'changed' RETURNING secret",
        "INSERT INTO join_view (id, value) VALUES (1, 'changed') RETURNING secret",
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42703"), "{sql}: {error}");
    }

    exec(
        engine,
        "CREATE VIEW tableoid_view AS
         SELECT id, tableoid AS source_tableoid FROM automatic_readonly_base",
    );
    let expected_oid = exec(
        engine,
        "SELECT 'automatic_readonly_base'::regclass AS source_tableoid",
    );
    let returned_oid = exec(
        engine,
        "UPDATE tableoid_view SET id = id WHERE id = 1 RETURNING source_tableoid",
    );
    assert_eq!(
        returned_oid.rows[0]["source_tableoid"],
        expected_oid.rows[0]["source_tableoid"]
    );
    let deleted = exec(
        engine,
        "DELETE FROM constant_only_view WHERE computed_id = 2 RETURNING computed_id",
    );
    assert_eq!(deleted.rows[0]["computed_id"], Value::Int(2));
    assert!(exec(engine, "SELECT * FROM automatic_readonly_base")
        .rows
        .is_empty());
    for sql in [
        "INSERT INTO automatic_materialized VALUES (1, 'one')",
        "UPDATE automatic_materialized SET value = 'changed'",
        "DELETE FROM automatic_materialized",
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42809"), "{sql}: {error}");
    }
}

fn assert_non_updatable_view_catalog_flags(engine: &Engine) {
    let views = exec(
        engine,
        "SELECT table_name, is_updatable, is_insertable_into, check_option
         FROM information_schema.views
         WHERE table_schema = 'public'
           AND table_name IN (
             'automatic_items', 'distinct_view', 'aggregate_view', 'join_view',
             'constant_only_view', 'set_returning_view', 'xmin_view'
           )
         ORDER BY table_name",
    );
    let flags = views
        .rows
        .iter()
        .map(|row| {
            (
                row["table_name"].clone(),
                row["is_updatable"].clone(),
                row["is_insertable_into"].clone(),
                row["check_option"].clone(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        flags,
        vec![
            (
                Value::Str("aggregate_view".into()),
                Value::Str("NO".into()),
                Value::Str("NO".into()),
                Value::Str("NONE".into()),
            ),
            (
                Value::Str("automatic_items".into()),
                Value::Str("YES".into()),
                Value::Str("YES".into()),
                Value::Str("NONE".into()),
            ),
            (
                Value::Str("constant_only_view".into()),
                Value::Str("NO".into()),
                Value::Str("NO".into()),
                Value::Str("NONE".into()),
            ),
            (
                Value::Str("distinct_view".into()),
                Value::Str("NO".into()),
                Value::Str("NO".into()),
                Value::Str("NONE".into()),
            ),
            (
                Value::Str("join_view".into()),
                Value::Str("NO".into()),
                Value::Str("NO".into()),
                Value::Str("NONE".into()),
            ),
            (
                Value::Str("set_returning_view".into()),
                Value::Str("NO".into()),
                Value::Str("NO".into()),
                Value::Str("NONE".into()),
            ),
            (
                Value::Str("xmin_view".into()),
                Value::Str("NO".into()),
                Value::Str("NO".into()),
                Value::Str("NONE".into()),
            ),
        ]
    );

    let columns = exec(
        engine,
        "SELECT column_name, is_updatable FROM information_schema.columns
         WHERE table_schema = 'public' AND table_name = 'automatic_items'
         ORDER BY ordinal_position",
    );
    assert_eq!(columns.rows[0]["is_updatable"], Value::Str("YES".into()));
    assert_eq!(columns.rows[1]["is_updatable"], Value::Str("YES".into()));
    assert_eq!(columns.rows[2]["is_updatable"], Value::Str("YES".into()));
    assert_eq!(columns.rows[3]["is_updatable"], Value::Str("NO".into()));
    let readonly_columns = exec(
        engine,
        "SELECT table_name, column_name, is_updatable
         FROM information_schema.columns
         WHERE table_schema = 'public'
           AND table_name IN ('constant_only_view', 'xmin_view')
         ORDER BY table_name, ordinal_position",
    );
    assert_eq!(readonly_columns.rows.len(), 2);
    assert!(readonly_columns
        .rows
        .iter()
        .all(|row| row["is_updatable"] == Value::Str("NO".into())));
}
