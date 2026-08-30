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
fn instead_of_view_merge_actions_and_statement_order_match_postgresql() {
    let engine = Engine::new();
    install_fixture(&engine);
    exec(
        &engine,
        "CREATE TABLE merge_source (id INTEGER, value TEXT);
         INSERT INTO merge_source VALUES (1, 'changed'), (3, 'three')",
    );
    let merged = exec(
        &engine,
        "MERGE INTO item_view AS target
         USING merge_source AS source ON target.id = source.id
         WHEN MATCHED THEN UPDATE SET value = source.value
         WHEN NOT MATCHED THEN INSERT (id, value) VALUES (source.id, source.value)
         WHEN NOT MATCHED BY SOURCE THEN DELETE
         RETURNING merge_action() AS action, source.id AS source_id,
           target.id, target.value, old.value AS old_value, new.value AS new_value",
    );
    assert_eq!(merged.affected_rows, 3);
    let update = merged
        .rows
        .iter()
        .find(|row| row["action"] == Value::Str("UPDATE".into()))
        .unwrap();
    assert_eq!(update["old_value"], Value::Str("one".into()));
    assert_eq!(update["new_value"], Value::Str("changed:a:returned".into()));
    let insert = merged
        .rows
        .iter()
        .find(|row| row["action"] == Value::Str("INSERT".into()))
        .unwrap();
    assert_eq!(insert["old_value"], Value::Null);
    assert_eq!(insert["new_value"], Value::Str("three:a:returned".into()));
    let delete = merged
        .rows
        .iter()
        .find(|row| row["action"] == Value::Str("DELETE".into()))
        .unwrap();
    assert_eq!(delete["source_id"], Value::Null);
    assert_eq!(delete["value"], Value::Str("two".into()));
    assert_eq!(
        strings(
            &engine,
            "SELECT entry FROM view_trigger_log
             WHERE entry LIKE 'BEFORE:%' OR entry LIKE 'AFTER:%'
             ORDER BY sequence",
            "entry",
        ),
        vec![
            "BEFORE:STATEMENT:INSERT",
            "BEFORE:STATEMENT:UPDATE",
            "BEFORE:STATEMENT:DELETE",
            "AFTER:STATEMENT:DELETE",
            "AFTER:STATEMENT:UPDATE",
            "AFTER:STATEMENT:INSERT",
        ]
    );
    assert_eq!(
        strings(
            &engine,
            "SELECT id::text || ':' || value AS item FROM view_base_items ORDER BY id",
            "item",
        ),
        vec!["1:changed:a:stored", "3:three:a:stored"]
    );
}

#[test]
fn instead_of_view_merge_allows_repeated_candidates_and_suppresses_null_results() {
    let engine = Engine::new();
    install_fixture(&engine);
    exec(
        &engine,
        "CREATE TABLE merge_source (id INTEGER, value TEXT);
         INSERT INTO merge_source VALUES (1, 'first'), (1, 'second')",
    );
    let repeated = exec(
        &engine,
        "MERGE INTO item_view AS target
         USING merge_source AS source ON target.id = source.id
         WHEN MATCHED THEN UPDATE SET value = source.value
         RETURNING merge_action() AS action, source.value AS source_value,
           old.value AS old_value, new.value AS new_value",
    );
    assert_eq!(repeated.affected_rows, 2);
    assert_eq!(repeated.rows.len(), 2);
    assert!(repeated
        .rows
        .iter()
        .all(|row| row["old_value"] == Value::Str("one".into())));
    assert_eq!(
        repeated.rows[0]["new_value"],
        Value::Str("first:a:returned".into())
    );
    assert_eq!(
        repeated.rows[1]["new_value"],
        Value::Str("second:a:returned".into())
    );
    assert_eq!(
        strings(
            &engine,
            "SELECT value FROM view_base_items WHERE id = 1",
            "value"
        ),
        vec!["second:a:stored"]
    );
    exec(
        &engine,
        "DELETE FROM view_trigger_log;
         TRUNCATE merge_source;
         INSERT INTO merge_source VALUES (4, 'suppress')",
    );
    let suppressed = exec(
        &engine,
        "MERGE INTO item_view AS target
         USING merge_source AS source ON target.id = source.id
         WHEN NOT MATCHED THEN INSERT (id, value) VALUES (source.id, source.value)
         RETURNING merge_action(), source.id, target.id",
    );
    assert_eq!(suppressed.affected_rows, 0);
    assert!(suppressed.rows.is_empty());
    assert_eq!(
        strings(
            &engine,
            "SELECT entry FROM view_trigger_log ORDER BY sequence",
            "entry"
        ),
        vec![
            "BEFORE:STATEMENT:INSERT",
            "a:INSERT:-:4",
            "AFTER:STATEMENT:INSERT",
        ]
    );
}

#[test]
fn view_merge_selects_one_complete_trigger_or_automatic_path() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE path_base (id INTEGER PRIMARY KEY, value INTEGER);
         INSERT INTO path_base VALUES (1, 10);
         CREATE TABLE path_source (id INTEGER, value INTEGER);
         INSERT INTO path_source VALUES (1, 20), (2, 30);
         CREATE VIEW path_view AS SELECT id, value FROM path_base;
         CREATE FUNCTION path_insert() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN INSERT INTO path_base VALUES (NEW.id, NEW.value + 1); RETURN NEW; END $$;
         CREATE TRIGGER path_insert INSTEAD OF INSERT ON path_view
           FOR EACH ROW EXECUTE FUNCTION path_insert()",
    );
    let mixed = engine
        .sql(
            "MERGE INTO path_view AS target USING path_source AS source
             ON target.id = source.id
             WHEN MATCHED THEN UPDATE SET value = source.value
             WHEN NOT MATCHED THEN INSERT (id, value) VALUES (source.id, source.value)",
            &[],
        )
        .expect_err("one MERGE cannot mix automatic and trigger action paths");
    assert_eq!(mixed.sqlstate(), Some("0A000"));
    assert_eq!(
        exec(
            &engine,
            "MERGE INTO path_view AS target USING path_source AS source
             ON target.id = source.id
             WHEN MATCHED THEN UPDATE SET value = source.value"
        )
        .affected_rows,
        1
    );
    assert_eq!(
        exec(&engine, "SELECT value FROM path_base WHERE id = 1").rows[0]["value"],
        Value::Int(20)
    );
    exec(
        &engine,
        "CREATE VIEW distinct_path AS SELECT DISTINCT id, value FROM path_base;
         CREATE TRIGGER distinct_update INSTEAD OF UPDATE ON distinct_path
           FOR EACH ROW EXECUTE FUNCTION path_insert()",
    );
    let unknown_column = engine
        .sql(
            "MERGE INTO distinct_path AS target USING path_source AS source
             ON target.id = source.id
             WHEN NOT MATCHED THEN INSERT (missing) VALUES (source.value)",
            &[],
        )
        .expect_err("the public view row type is validated before trigger capability");
    assert_eq!(unknown_column.sqlstate(), Some("42703"));
    let missing = engine
        .sql(
            "MERGE INTO distinct_path AS target USING path_source AS source
             ON target.id = source.id
             WHEN NOT MATCHED THEN INSERT (id, value) VALUES (source.id, source.value)",
            &[],
        )
        .expect_err("a nonautomatic action needs its own INSTEAD OF trigger");
    assert_eq!(missing.sqlstate(), Some("55000"));
    exec(
        &engine,
        "CREATE VIEW read_only_path AS SELECT DISTINCT id, value FROM path_base",
    );
    assert_eq!(
        exec(
            &engine,
            "MERGE INTO read_only_path AS target USING path_source AS source
             ON target.id = source.id WHEN MATCHED THEN DO NOTHING"
        )
        .affected_rows,
        0
    );
}

#[test]
fn view_merge_trigger_definitions_route_even_when_replica_mode_suppresses_them() {
    let engine = Engine::new();
    install_fixture(&engine);
    exec(
        &engine,
        "CREATE TABLE merge_source (id INTEGER, value TEXT);
         INSERT INTO merge_source VALUES (1, 'changed');
         DELETE FROM view_trigger_log;
         SET session_replication_role = replica",
    );
    let result = exec(
        &engine,
        "MERGE INTO item_view AS target USING merge_source AS source
         ON target.id = source.id
         WHEN MATCHED THEN UPDATE SET value = source.value
         RETURNING merge_action(), old.value, new.value",
    );
    assert_eq!(result.affected_rows, 0);
    assert!(result.rows.is_empty());
    assert_eq!(
        strings(
            &engine,
            "SELECT value FROM view_base_items WHERE id = 1",
            "value"
        ),
        vec!["one"]
    );
    assert!(exec(&engine, "SELECT entry FROM view_trigger_log")
        .rows
        .is_empty());
    exec(&engine, "RESET session_replication_role");
}

#[test]
fn nested_automatic_view_merge_uses_inner_triggers_and_final_check_options() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE nested_merge_base (id INTEGER PRIMARY KEY, value INTEGER);
         INSERT INTO nested_merge_base VALUES (1, 10), (2, 150);
         CREATE TABLE nested_merge_source (id INTEGER, value INTEGER);
         INSERT INTO nested_merge_source VALUES (1, 20);
         CREATE VIEW nested_merge_inner AS SELECT id, value FROM nested_merge_base;
         CREATE FUNCTION nested_merge_apply() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           IF TG_OP = 'UPDATE' THEN
             NEW.value := NEW.value + 10;
             UPDATE nested_merge_base SET value = NEW.value WHERE id = OLD.id;
             RETURN NEW;
           ELSIF TG_OP = 'INSERT' THEN
             INSERT INTO nested_merge_base VALUES (NEW.id, NEW.value);
             RETURN NEW;
           END IF;
           DELETE FROM nested_merge_base WHERE id = OLD.id;
           RETURN OLD;
         END $$;
         CREATE TRIGGER nested_merge_apply INSTEAD OF INSERT OR UPDATE OR DELETE
           ON nested_merge_inner FOR EACH ROW EXECUTE FUNCTION nested_merge_apply();
         CREATE VIEW nested_merge_outer (item_id, amount) AS
           SELECT id, value FROM nested_merge_inner WHERE value < 100
           WITH CASCADED CHECK OPTION",
    );
    let updated = exec(
        &engine,
        "MERGE INTO nested_merge_outer AS target USING nested_merge_source AS source
         ON target.item_id = source.id
         WHEN MATCHED THEN UPDATE SET amount = source.value
         RETURNING merge_action(), source.id, target.item_id,
           old.amount AS old_amount, new.amount AS new_amount",
    );
    assert_eq!(updated.affected_rows, 1);
    assert_eq!(updated.rows[0]["old_amount"], Value::Int(10));
    assert_eq!(updated.rows[0]["new_amount"], Value::Int(30));
    exec(
        &engine,
        "UPDATE nested_merge_source SET value = 95 WHERE id = 1",
    );
    let check = engine
        .sql(
            "MERGE INTO nested_merge_outer AS target USING nested_merge_source AS source
             ON target.item_id = source.id
             WHEN MATCHED THEN UPDATE SET amount = source.value",
            &[],
        )
        .expect_err("the outer check option sees the inner trigger's returned NEW row");
    assert_eq!(check.sqlstate(), Some("44000"));
    let state = exec(
        &engine,
        "SELECT id, value FROM nested_merge_base ORDER BY id",
    );
    assert_eq!(state.rows[0]["value"], Value::Int(30));
    assert_eq!(state.rows[1]["value"], Value::Int(150));
}

#[test]
fn view_merge_keeps_source_and_target_on_the_pre_statement_trigger_snapshot() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE snapshot_merge_base (id INTEGER PRIMARY KEY, value TEXT);
         CREATE TABLE snapshot_merge_source (id INTEGER PRIMARY KEY, value TEXT);
         CREATE VIEW snapshot_merge_view AS SELECT id, value FROM snapshot_merge_base;
         CREATE FUNCTION seed_snapshot_merge() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           INSERT INTO snapshot_merge_base VALUES (1, 'target');
           INSERT INTO snapshot_merge_source VALUES (1, 'source');
           RETURN NULL;
         END $$;
         CREATE FUNCTION apply_snapshot_merge() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN UPDATE snapshot_merge_base SET value = NEW.value WHERE id = OLD.id; RETURN NEW; END $$;
         CREATE TRIGGER seed_snapshot_merge BEFORE UPDATE ON snapshot_merge_view
           FOR EACH STATEMENT EXECUTE FUNCTION seed_snapshot_merge();
         CREATE TRIGGER apply_snapshot_merge INSTEAD OF UPDATE ON snapshot_merge_view
           FOR EACH ROW EXECUTE FUNCTION apply_snapshot_merge()",
    );
    let merged = exec(
        &engine,
        "MERGE INTO snapshot_merge_view AS target USING snapshot_merge_source AS source
         ON target.id = source.id
         WHEN MATCHED THEN UPDATE SET value = source.value
         RETURNING merge_action(), target.id, target.value",
    );
    assert_eq!(merged.affected_rows, 0);
    assert!(merged.rows.is_empty());
    assert_eq!(
        strings(
            &engine,
            "SELECT value FROM snapshot_merge_base ORDER BY id",
            "value"
        ),
        vec!["target"]
    );
}

#[test]
fn view_merge_action_subqueries_keep_the_statement_snapshot_across_row_triggers() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE action_snapshot_base (id INTEGER PRIMARY KEY, value INTEGER);
         INSERT INTO action_snapshot_base VALUES (1, 10), (2, 20);
         CREATE TABLE action_snapshot_source (id INTEGER);
         INSERT INTO action_snapshot_source VALUES (1), (2);
         CREATE VIEW action_snapshot_view AS
           SELECT DISTINCT id, value FROM action_snapshot_base;
         CREATE FUNCTION apply_action_snapshot() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           UPDATE action_snapshot_base SET value = NEW.value WHERE id = OLD.id;
           RETURN NEW;
         END $$;
         CREATE TRIGGER apply_action_snapshot INSTEAD OF UPDATE ON action_snapshot_view
           FOR EACH ROW EXECUTE FUNCTION apply_action_snapshot()",
    );
    let merged = exec(
        &engine,
        "MERGE INTO action_snapshot_view AS target USING action_snapshot_source AS source
         ON target.id = source.id
         WHEN MATCHED AND (SELECT max(value) FROM action_snapshot_base) = 20
           THEN UPDATE SET value = (SELECT max(value) + 1 FROM action_snapshot_base)
         RETURNING target.id, old.value AS old_value, new.value AS new_value",
    );
    assert_eq!(merged.affected_rows, 2);
    assert_eq!(merged.rows.len(), 2);
    assert_eq!(merged.rows[0]["old_value"], Value::Int(10));
    assert_eq!(merged.rows[1]["old_value"], Value::Int(20));
    assert!(merged
        .rows
        .iter()
        .all(|row| row["new_value"] == Value::Int(21)));
    assert_eq!(
        exec(
            &engine,
            "SELECT value FROM action_snapshot_base ORDER BY id"
        )
        .rows
        .iter()
        .map(|row| row["value"].clone())
        .collect::<Vec<_>>(),
        vec![Value::Int(21), Value::Int(21)]
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
