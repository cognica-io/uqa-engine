//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 `INSTEAD OF` view-trigger execution and lifecycle coverage.

use tempfile::TempDir;
use uqa_core::Value;
use uqa_engine::Engine;

use super::{exec, strings};

fn install_fixture(engine: &Engine) {
    for sql in [
        "CREATE TABLE view_base_items (id INTEGER PRIMARY KEY, value TEXT NOT NULL)",
        "INSERT INTO view_base_items VALUES (1, 'one'), (2, 'two')",
        "CREATE VIEW item_view AS SELECT id, value FROM view_base_items WHERE id > 0",
        "CREATE TABLE view_trigger_log (sequence BIGSERIAL PRIMARY KEY, entry TEXT)",
        "CREATE FUNCTION view_statement_log() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO view_trigger_log(entry) VALUES (TG_WHEN || ':' || TG_LEVEL || ':' || TG_OP); RETURN NULL; END $$",
        "CREATE FUNCTION view_transform_row() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO view_trigger_log(entry) VALUES ('a:' || TG_OP || ':' || coalesce(OLD.id::text, '-') || ':' || coalesce(NEW.id::text, '-')); IF TG_OP <> 'DELETE' AND NEW.value = 'suppress' THEN RETURN NULL; END IF; IF TG_OP <> 'DELETE' THEN NEW.value := NEW.value || ':a'; RETURN NEW; END IF; OLD.value := OLD.value || ':a'; RETURN OLD; END $$",
        "CREATE FUNCTION view_apply_row() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO view_trigger_log(entry) VALUES ('b:' || TG_OP || ':' || coalesce(OLD.value, '-') || ':' || coalesce(NEW.value, '-')); IF TG_OP = 'INSERT' THEN INSERT INTO view_base_items VALUES (NEW.id, NEW.value || ':stored'); NEW.value := NEW.value || ':returned'; RETURN NEW; ELSIF TG_OP = 'UPDATE' THEN UPDATE view_base_items SET id = NEW.id, value = NEW.value || ':stored' WHERE id = OLD.id; NEW.value := NEW.value || ':returned'; RETURN NEW; ELSE DELETE FROM view_base_items WHERE id = OLD.id; OLD.value := OLD.value || ':returned'; RETURN OLD; END IF; END $$",
        "CREATE TRIGGER before_insert BEFORE INSERT ON item_view FOR EACH STATEMENT EXECUTE FUNCTION view_statement_log()",
        "CREATE TRIGGER before_update BEFORE UPDATE ON item_view FOR EACH STATEMENT EXECUTE FUNCTION view_statement_log()",
        "CREATE TRIGGER before_delete BEFORE DELETE ON item_view FOR EACH STATEMENT EXECUTE FUNCTION view_statement_log()",
        "CREATE TRIGGER a_transform INSTEAD OF INSERT OR UPDATE OR DELETE ON item_view FOR EACH ROW EXECUTE FUNCTION view_transform_row()",
        "CREATE TRIGGER b_apply INSTEAD OF INSERT OR UPDATE OR DELETE ON item_view FOR EACH ROW EXECUTE FUNCTION view_apply_row()",
        "CREATE TRIGGER after_insert AFTER INSERT ON item_view FOR EACH STATEMENT EXECUTE FUNCTION view_statement_log()",
        "CREATE TRIGGER after_update AFTER UPDATE ON item_view FOR EACH STATEMENT EXECUTE FUNCTION view_statement_log()",
        "CREATE TRIGGER after_delete AFTER DELETE ON item_view FOR EACH STATEMENT EXECUTE FUNCTION view_statement_log()",
    ] {
        exec(engine, sql);
    }
}

#[test]
fn instead_of_trigger_definition_validation_and_catalog_match_postgresql() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE base_items (id INTEGER)");
    exec(
        &engine,
        "CREATE VIEW item_view AS SELECT id FROM base_items",
    );
    exec(
        &engine,
        "CREATE FUNCTION view_probe() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$",
    );
    for (sql, state) in [
        (
            "CREATE TRIGGER bad INSTEAD OF INSERT ON base_items FOR EACH ROW EXECUTE FUNCTION view_probe()",
            "42809",
        ),
        (
            "CREATE TRIGGER bad INSTEAD OF INSERT ON item_view FOR EACH STATEMENT EXECUTE FUNCTION view_probe()",
            "0A000",
        ),
        (
            "CREATE TRIGGER bad INSTEAD OF INSERT ON item_view FOR EACH ROW WHEN (NEW.id > 0) EXECUTE FUNCTION view_probe()",
            "0A000",
        ),
        (
            "CREATE TRIGGER bad INSTEAD OF UPDATE OF id ON item_view FOR EACH ROW EXECUTE FUNCTION view_probe()",
            "0A000",
        ),
        (
            "CREATE TRIGGER bad BEFORE INSERT ON item_view FOR EACH ROW EXECUTE FUNCTION view_probe()",
            "42809",
        ),
    ] {
        let error = engine.sql(sql, &[]).expect_err("invalid view trigger must fail");
        assert_eq!(error.sqlstate(), Some(state), "{sql}: {error}");
    }
    exec(
        &engine,
        "CREATE TRIGGER view_instead INSTEAD OF INSERT OR UPDATE OR DELETE ON item_view FOR EACH ROW EXECUTE FUNCTION view_probe()",
    );
    let row = exec(
        &engine,
        "SELECT t.tgtype, t.tgenabled, c.relhastriggers, c.relhasrules, pg_get_triggerdef(t.oid, false) AS definition FROM pg_trigger AS t JOIN pg_class AS c ON c.oid = t.tgrelid WHERE t.tgname = 'view_instead'",
    )
    .rows
    .into_iter()
    .next()
    .expect("view trigger catalog row");
    assert_eq!(row.get("tgtype"), Some(&Value::Int(93)));
    assert_eq!(row.get("tgenabled"), Some(&Value::Str("O".into())));
    assert_eq!(row.get("relhastriggers"), Some(&Value::Bool(true)));
    assert_eq!(row.get("relhasrules"), Some(&Value::Bool(true)));
    assert_eq!(
        row.get("definition"),
        Some(&Value::Str(
            "CREATE TRIGGER view_instead INSTEAD OF INSERT OR DELETE OR UPDATE ON public.item_view FOR EACH ROW EXECUTE FUNCTION view_probe()".into()
        ))
    );
    let disable = engine
        .sql("ALTER TABLE item_view DISABLE TRIGGER view_instead", &[])
        .expect_err("view trigger enable modes must be rejected");
    assert_eq!(disable.sqlstate(), Some("42809"));
    exec(
        &engine,
        "ALTER TRIGGER view_instead ON item_view RENAME TO renamed_instead",
    );
    let renamed = exec(
        &engine,
        "SELECT tgname FROM pg_trigger WHERE tgrelid = 'item_view'::regclass",
    );
    assert_eq!(
        renamed.rows[0].get("tgname"),
        Some(&Value::Str("renamed_instead".into()))
    );
    exec(&engine, "DROP TRIGGER renamed_instead ON item_view");
    let remaining = exec(
        &engine,
        "SELECT count(*) AS n FROM pg_trigger WHERE tgrelid = 'item_view'::regclass",
    );
    assert_eq!(remaining.rows[0].get("n"), Some(&Value::Int(0)));
}

#[test]
fn instead_of_row_chain_suppression_returning_and_statement_order_match_postgresql() {
    let engine = Engine::new();
    install_fixture(&engine);

    let inserted = exec(
        &engine,
        "INSERT INTO item_view VALUES (3, 'three'), (4, 'suppress') RETURNING WITH (OLD AS o, NEW AS n) o.id AS old_id, n.id AS new_id, id, value",
    );
    assert_eq!(inserted.affected_rows, 1);
    assert_eq!(inserted.rows.len(), 1);
    assert_eq!(inserted.rows[0].get("old_id"), Some(&Value::Null));
    assert_eq!(inserted.rows[0].get("new_id"), Some(&Value::Int(3)));
    assert_eq!(
        inserted.rows[0].get("value"),
        Some(&Value::Str("three:a:returned".into()))
    );

    let updated = exec(
        &engine,
        "UPDATE item_view SET id = id + 10, value = value || ':updated' WHERE id IN (1, 2) RETURNING WITH (OLD AS o, NEW AS n) o.id AS old_id, o.value AS old_value, n.id AS new_id, n.value AS new_value, id, value",
    );
    assert_eq!(updated.affected_rows, 2);
    assert_eq!(updated.rows.len(), 2);
    assert_eq!(updated.rows[0].get("old_id"), Some(&Value::Int(1)));
    assert_eq!(updated.rows[0].get("new_id"), Some(&Value::Int(11)));
    assert_eq!(
        updated.rows[0].get("new_value"),
        Some(&Value::Str("one:updated:a:returned".into()))
    );

    let deleted = exec(
        &engine,
        "DELETE FROM item_view WHERE id = 11 RETURNING WITH (OLD AS o, NEW AS n) o.value AS old_value, n.value AS new_value, id, value",
    );
    assert_eq!(deleted.affected_rows, 1);
    assert_eq!(deleted.rows[0].get("id"), Some(&Value::Int(11)));
    assert_eq!(
        deleted.rows[0].get("value"),
        Some(&Value::Str("one:updated:a:stored".into()))
    );
    assert_eq!(deleted.rows[0].get("new_value"), Some(&Value::Null));

    assert_eq!(
        strings(
            &engine,
            "SELECT entry FROM view_trigger_log ORDER BY sequence",
            "entry",
        ),
        vec![
            "BEFORE:STATEMENT:INSERT",
            "a:INSERT:-:3",
            "b:INSERT:-:three:a",
            "a:INSERT:-:4",
            "AFTER:STATEMENT:INSERT",
            "BEFORE:STATEMENT:UPDATE",
            "a:UPDATE:1:11",
            "b:UPDATE:one:one:updated:a",
            "a:UPDATE:2:12",
            "b:UPDATE:two:two:updated:a",
            "AFTER:STATEMENT:UPDATE",
            "BEFORE:STATEMENT:DELETE",
            "a:DELETE:11:-",
            "b:DELETE:one:updated:a:stored:-",
            "AFTER:STATEMENT:DELETE",
        ]
    );
    assert_eq!(
        strings(
            &engine,
            "SELECT id::text || ':' || value AS item FROM view_base_items ORDER BY id",
            "item",
        ),
        vec!["3:three:a:stored", "12:two:updated:a:stored"]
    );

    exec(&engine, "DELETE FROM view_trigger_log");
    let zero = exec(&engine, "UPDATE item_view SET value = value WHERE id = 999");
    assert_eq!(zero.affected_rows, 0);
    assert_eq!(
        strings(
            &engine,
            "SELECT entry FROM view_trigger_log ORDER BY sequence",
            "entry",
        ),
        vec!["BEFORE:STATEMENT:UPDATE", "AFTER:STATEMENT:UPDATE"]
    );
}

#[test]
fn instead_of_update_from_and_delete_using_preserve_source_context() {
    let engine = Engine::new();
    install_fixture(&engine);
    exec(
        &engine,
        "CREATE TABLE view_source (id INTEGER PRIMARY KEY, next_value TEXT)",
    );
    exec(
        &engine,
        "INSERT INTO view_source VALUES (1, 'changed'), (2, 'remove')",
    );
    let updated = exec(
        &engine,
        "UPDATE item_view AS target SET value = source.next_value FROM view_source AS source WHERE target.id = source.id AND source.id = 1 RETURNING target.id, source.next_value AS source_value, value",
    );
    assert_eq!(updated.affected_rows, 1);
    assert_eq!(
        updated.rows[0].get("source_value"),
        Some(&Value::Str("changed".into()))
    );
    assert_eq!(
        updated.rows[0].get("value"),
        Some(&Value::Str("changed:a:returned".into()))
    );
    let deleted = exec(
        &engine,
        "DELETE FROM item_view AS target USING view_source AS source WHERE target.id = source.id AND source.id = 2 RETURNING target.id, source.id AS source_id, value",
    );
    assert_eq!(deleted.affected_rows, 1);
    assert_eq!(deleted.rows[0].get("source_id"), Some(&Value::Int(2)));
    assert_eq!(
        deleted.rows[0].get("value"),
        Some(&Value::Str("two".into()))
    );
}

#[test]
fn view_dml_target_resolution_respects_the_shared_relation_namespace() {
    let engine = Engine::new();
    for sql in [
        "CREATE SCHEMA first_schema",
        "CREATE SCHEMA second_schema",
        "CREATE TABLE first_schema.shadowed_items (id INTEGER PRIMARY KEY, value TEXT)",
        "CREATE TABLE second_schema.view_base (id INTEGER PRIMARY KEY, value TEXT)",
        "CREATE VIEW second_schema.shadowed_items AS SELECT id, value FROM second_schema.view_base",
        "CREATE FUNCTION second_schema.apply_shadowed_item() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO second_schema.view_base VALUES (NEW.id, NEW.value); RETURN NEW; END $$",
        "CREATE TRIGGER apply_shadowed INSTEAD OF INSERT ON second_schema.shadowed_items FOR EACH ROW EXECUTE FUNCTION second_schema.apply_shadowed_item()",
    ] {
        exec(&engine, sql);
    }
    exec(&engine, "SET search_path = first_schema, second_schema");
    exec(&engine, "INSERT INTO shadowed_items VALUES (1, 'table')");
    exec(&engine, "SET search_path = second_schema, first_schema");
    exec(&engine, "INSERT INTO shadowed_items VALUES (2, 'view')");
    assert_eq!(
        strings(
            &engine,
            "SELECT value FROM first_schema.shadowed_items ORDER BY id",
            "value",
        ),
        vec!["table"]
    );
    assert_eq!(
        strings(
            &engine,
            "SELECT value FROM second_schema.view_base ORDER BY id",
            "value",
        ),
        vec!["view"]
    );
}

#[test]
fn insert_select_keeps_the_statement_snapshot_across_before_statement_triggers() {
    let engine = Engine::new();
    for sql in [
        "CREATE TABLE snapshot_base (id INTEGER PRIMARY KEY, value TEXT)",
        "CREATE VIEW snapshot_view AS SELECT id, value FROM snapshot_base",
        "CREATE TABLE snapshot_source (id INTEGER PRIMARY KEY, value TEXT)",
        "CREATE FUNCTION seed_snapshot_source() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO snapshot_source VALUES (1, 'seeded'); RETURN NULL; END $$",
        "CREATE FUNCTION apply_snapshot_row() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO snapshot_base VALUES (NEW.id, NEW.value); RETURN NEW; END $$",
        "CREATE TRIGGER seed_source BEFORE INSERT ON snapshot_view FOR EACH STATEMENT EXECUTE FUNCTION seed_snapshot_source()",
        "CREATE TRIGGER apply_row INSTEAD OF INSERT ON snapshot_view FOR EACH ROW EXECUTE FUNCTION apply_snapshot_row()",
    ] {
        exec(&engine, sql);
    }
    let inserted = exec(
        &engine,
        "INSERT INTO snapshot_view SELECT id, value FROM snapshot_source RETURNING id, value",
    );
    assert_eq!(inserted.affected_rows, 0);
    assert!(inserted.rows.is_empty());
    assert_eq!(
        strings(
            &engine,
            "SELECT value FROM snapshot_source ORDER BY id",
            "value",
        ),
        vec!["seeded"]
    );
    assert!(exec(&engine, "SELECT id FROM snapshot_base")
        .rows
        .is_empty());
    for sql in [
        "CREATE FUNCTION seed_snapshot_update() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO snapshot_base VALUES (2, 'update-seeded'); RETURN NULL; END $$",
        "CREATE FUNCTION seed_snapshot_delete() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO snapshot_base VALUES (3, 'delete-seeded'); RETURN NULL; END $$",
        "CREATE FUNCTION apply_snapshot_change() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF TG_OP = 'UPDATE' THEN UPDATE snapshot_base SET value = NEW.value WHERE id = OLD.id; RETURN NEW; END IF; DELETE FROM snapshot_base WHERE id = OLD.id; RETURN OLD; END $$",
        "CREATE TRIGGER seed_update BEFORE UPDATE ON snapshot_view FOR EACH STATEMENT EXECUTE FUNCTION seed_snapshot_update()",
        "CREATE TRIGGER seed_delete BEFORE DELETE ON snapshot_view FOR EACH STATEMENT EXECUTE FUNCTION seed_snapshot_delete()",
        "CREATE TRIGGER apply_change INSTEAD OF UPDATE OR DELETE ON snapshot_view FOR EACH ROW EXECUTE FUNCTION apply_snapshot_change()",
    ] {
        exec(&engine, sql);
    }
    let updated = exec(
        &engine,
        "UPDATE snapshot_view SET value = 'updated' RETURNING id, value",
    );
    assert_eq!(updated.affected_rows, 0);
    assert!(updated.rows.is_empty());
    assert_eq!(
        strings(
            &engine,
            "SELECT value FROM snapshot_base WHERE id = 2",
            "value",
        ),
        vec!["update-seeded"]
    );
    let deleted = exec(&engine, "DELETE FROM snapshot_view RETURNING id, value");
    assert_eq!(deleted.affected_rows, 1);
    assert_eq!(deleted.rows[0].get("id"), Some(&Value::Int(2)));
    assert_eq!(
        strings(
            &engine,
            "SELECT value FROM snapshot_base ORDER BY id",
            "value",
        ),
        vec!["delete-seeded"]
    );
}

#[test]
fn instead_of_view_trigger_survives_reopen() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("instead-of-view-trigger.db");
    {
        let engine = Engine::open(&database).unwrap();
        install_fixture(&engine);
        exec(&engine, "INSERT INTO item_view VALUES (3, 'before')");
    }
    let engine = Engine::open(&database).unwrap();
    let inserted = exec(
        &engine,
        "INSERT INTO item_view VALUES (4, 'after') RETURNING value",
    );
    assert_eq!(inserted.affected_rows, 1);
    assert_eq!(
        inserted.rows[0].get("value"),
        Some(&Value::Str("after:a:returned".into()))
    );
    assert_eq!(
        strings(
            &engine,
            "SELECT value FROM view_base_items WHERE id IN (3, 4) ORDER BY id",
            "value",
        ),
        vec!["before:a:stored", "after:a:stored"]
    );
}
