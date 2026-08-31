//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

pub(super) fn assert_base_triggers_replace_view_statement_triggers() {
    let engine = automatic_view_engine();
    exec(
        &engine,
        "CREATE TABLE automatic_log (
            seq BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
            entry TEXT NOT NULL
        )",
    );
    exec(
        &engine,
        "CREATE FUNCTION automatic_log_trigger() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           INSERT INTO automatic_log(entry) VALUES
             (TG_TABLE_NAME || ':' || TG_WHEN || ':' || TG_LEVEL || ':' || TG_OP);
           RETURN NEW;
         END
         $$",
    );
    exec(
        &engine,
        "CREATE TRIGGER view_before BEFORE INSERT ON automatic_items
         FOR EACH STATEMENT EXECUTE FUNCTION automatic_log_trigger()",
    );
    exec(
        &engine,
        "CREATE TRIGGER view_after AFTER INSERT ON automatic_items
         FOR EACH STATEMENT EXECUTE FUNCTION automatic_log_trigger()",
    );
    exec(
        &engine,
        "CREATE TRIGGER base_before BEFORE INSERT ON automatic_base
         FOR EACH STATEMENT EXECUTE FUNCTION automatic_log_trigger()",
    );
    exec(
        &engine,
        "CREATE TRIGGER base_after AFTER INSERT ON automatic_base
         FOR EACH STATEMENT EXECUTE FUNCTION automatic_log_trigger()",
    );

    exec(
        &engine,
        "INSERT INTO automatic_items (item_id, label) VALUES (1, 'one')",
    );
    let log = exec(&engine, "SELECT entry FROM automatic_log ORDER BY seq");
    assert_eq!(
        log.rows
            .iter()
            .map(|row| row["entry"].clone())
            .collect::<Vec<_>>(),
        vec![
            Value::Str("automatic_base:BEFORE:STATEMENT:INSERT".into()),
            Value::Str("automatic_base:AFTER:STATEMENT:INSERT".into()),
        ]
    );

    exec(
        &engine,
        "CREATE FUNCTION automatic_noop_view_trigger() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           RETURN NEW;
         END
         $$",
    );
    exec(
        &engine,
        "CREATE TRIGGER automatic_instead_insert INSTEAD OF INSERT ON automatic_items
         FOR EACH ROW EXECUTE FUNCTION automatic_noop_view_trigger()",
    );
    exec(&engine, "SET session_replication_role = replica");
    let catalog = exec(
        &engine,
        "SELECT is_trigger_insertable_into FROM information_schema.views
         WHERE table_schema = 'public' AND table_name = 'automatic_items'",
    );
    assert_eq!(
        catalog.rows[0]["is_trigger_insertable_into"],
        Value::Str("YES".into())
    );
    let suppressed = exec(
        &engine,
        "INSERT INTO automatic_items (item_id, label) VALUES (99, 'suppressed') RETURNING *",
    );
    assert_eq!(suppressed.affected_rows, 0);
    assert!(suppressed.rows.is_empty());
    exec(&engine, "RESET session_replication_role");
    assert!(exec(&engine, "SELECT * FROM automatic_base WHERE id = 99")
        .rows
        .is_empty());
    assert_persistent_batch_can_create_check_trigger_after_routine_write();
}

fn assert_check_options_after_before_triggers(engine: &Engine) {
    exec(
        engine,
        "CREATE FUNCTION hide_checked_row() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           IF NEW.value LIKE 'hide%' THEN
             NEW.visible := false;
           END IF;
           RETURN NEW;
         END
         $$",
    );
    exec(
        engine,
        "CREATE TRIGGER hide_checked_row BEFORE INSERT OR UPDATE ON automatic_base
         FOR EACH ROW EXECUTE FUNCTION hide_checked_row()",
    );
    let before_insert = engine
        .sql(
            "INSERT INTO automatic_local (id, value) VALUES (11, 'hide-insert')",
            &[],
        )
        .unwrap_err();
    assert_eq!(before_insert.sqlstate(), Some("44000"));
    assert!(exec(engine, "SELECT * FROM automatic_base WHERE id = 11")
        .rows
        .is_empty());
    let multirow = engine
        .sql(
            "INSERT INTO automatic_local (id, value, visible)
             VALUES (11, 'eleven', true), (12, 'twelve', false)",
            &[],
        )
        .unwrap_err();
    assert_eq!(multirow.sqlstate(), Some("44000"));
    assert_eq!(
        exec(
            engine,
            "SELECT count(*) AS total FROM automatic_base WHERE id IN (11, 12)"
        )
        .rows[0]["total"],
        Value::Int(0)
    );
    exec(
        engine,
        "INSERT INTO automatic_local (id, value) VALUES (11, 'eleven')",
    );
    let before_update = engine
        .sql(
            "UPDATE automatic_local SET value = 'hide-update' WHERE id = 11",
            &[],
        )
        .unwrap_err();
    assert_eq!(before_update.sqlstate(), Some("44000"));
    let conflict_update = engine
        .sql(
            "INSERT INTO automatic_local (id, value) VALUES (11, 'hide-conflict')
             ON CONFLICT (id) DO UPDATE SET value = excluded.value",
            &[],
        )
        .unwrap_err();
    assert_eq!(conflict_update.sqlstate(), Some("44000"));
    let unchanged = exec(
        engine,
        "SELECT value, visible FROM automatic_base WHERE id = 11",
    );
    assert_eq!(unchanged.rows[0]["value"], Value::Str("eleven".into()));
    assert_eq!(unchanged.rows[0]["visible"], Value::Bool(true));
}

pub(super) fn assert_local_and_cascaded_check_options() {
    let engine = automatic_view_engine();
    exec(
        &engine,
        "CREATE VIEW automatic_local AS
         SELECT id, value, visible, quantity FROM automatic_base WHERE visible
         WITH LOCAL CHECK OPTION",
    );

    let rejected_insert = engine
        .sql(
            "INSERT INTO automatic_local (id, value, visible) VALUES (10, 'ten', false)",
            &[],
        )
        .unwrap_err();
    assert_eq!(rejected_insert.sqlstate(), Some("44000"));
    assert!(exec(&engine, "SELECT * FROM automatic_base WHERE id = 10")
        .rows
        .is_empty());

    exec(
        &engine,
        "INSERT INTO automatic_local (id, value) VALUES (10, 'ten')",
    );
    let rejected_update = engine
        .sql(
            "UPDATE automatic_local SET visible = false WHERE id = 10",
            &[],
        )
        .unwrap_err();
    assert_eq!(rejected_update.sqlstate(), Some("44000"));
    assert_eq!(
        exec(&engine, "SELECT visible FROM automatic_base WHERE id = 10").rows[0]["visible"],
        Value::Bool(true)
    );
    exec(
        &engine,
        "CREATE TABLE automatic_check_source (id INTEGER PRIMARY KEY)",
    );
    exec(&engine, "INSERT INTO automatic_check_source VALUES (10)");
    let rejected_update_from = engine
        .sql(
            "UPDATE automatic_local AS target SET visible = false
             FROM automatic_check_source AS source
             WHERE target.id = source.id AND source.id = 10",
            &[],
        )
        .unwrap_err();
    assert_eq!(rejected_update_from.sqlstate(), Some("44000"));
    assert_eq!(
        exec(&engine, "SELECT visible FROM automatic_base WHERE id = 10").rows[0]["visible"],
        Value::Bool(true)
    );

    assert_check_options_after_before_triggers(&engine);
    assert_nested_check_options(&engine);
}

fn assert_nested_check_options(engine: &Engine) {
    exec(
        engine,
        "CREATE TABLE nested_base (
            id INTEGER PRIMARY KEY,
            inner_ok BOOLEAN NOT NULL DEFAULT true,
            outer_ok BOOLEAN NOT NULL DEFAULT true,
            note TEXT NOT NULL DEFAULT 'defaulted'
        )",
    );
    exec(
        engine,
        "CREATE VIEW inner_open AS SELECT * FROM nested_base WHERE inner_ok",
    );
    exec(
        engine,
        "CREATE VIEW outer_local AS SELECT * FROM inner_open WHERE outer_ok WITH LOCAL CHECK OPTION",
    );
    exec(
        engine,
        "CREATE VIEW outer_cascaded AS SELECT * FROM inner_open WHERE outer_ok WITH CASCADED CHECK OPTION",
    );
    exec(
        engine,
        "INSERT INTO outer_local (id, inner_ok, outer_ok) VALUES (20, false, true)",
    );
    assert_eq!(
        exec(engine, "SELECT inner_ok FROM nested_base WHERE id = 20").rows[0]["inner_ok"],
        Value::Bool(false)
    );
    let cascaded = engine
        .sql(
            "INSERT INTO outer_cascaded (id, inner_ok, outer_ok) VALUES (21, false, true)",
            &[],
        )
        .unwrap_err();
    assert_eq!(cascaded.sqlstate(), Some("44000"));

    exec(
        engine,
        "CREATE VIEW ordered_inner AS
         SELECT * FROM nested_base WHERE inner_ok;
         CREATE VIEW ordered_outer AS
         SELECT * FROM ordered_inner WHERE 10 / id > 0 WITH CASCADED CHECK OPTION",
    );
    let ordered = engine
        .sql(
            "INSERT INTO ordered_outer (id, inner_ok, outer_ok) VALUES (0, false, true)",
            &[],
        )
        .unwrap_err();
    assert_eq!(
        ordered.sqlstate(),
        Some("44000"),
        "the inner view check must run before the outer division"
    );
}

fn assert_persistent_batch_can_create_check_trigger_after_routine_write() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("automatic-view-check-trigger.db");
    let engine = Engine::open(&path).unwrap();
    exec(
        &engine,
        "CREATE SCHEMA checked; CREATE TABLE checked.base_items (
            id INTEGER PRIMARY KEY,
            value TEXT NOT NULL,
            visible BOOLEAN NOT NULL DEFAULT true
        ); CREATE VIEW checked.items AS
            SELECT id, value, visible FROM checked.base_items WHERE visible
            WITH LOCAL CHECK OPTION",
    );
    exec(
        &engine,
        "SET search_path = checked, pg_catalog;
         CREATE FUNCTION hide_checked_row() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           IF NEW.value LIKE 'hide%' THEN
             NEW.visible := false;
           END IF;
           RETURN NEW;
         END
         $$;
         CREATE TRIGGER hide_checked_row BEFORE INSERT OR UPDATE ON base_items
         FOR EACH ROW EXECUTE FUNCTION hide_checked_row()",
    );

    let error = engine
        .sql(
            "INSERT INTO checked.items (id, value) VALUES (1, 'hide-insert')",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("44000"));
    assert!(exec(&engine, "SELECT * FROM checked.base_items")
        .rows
        .is_empty());
}
