//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 row and statement trigger lifecycle coverage.

use tempfile::TempDir;
use uqa_core::Value;
use uqa_engine::Engine;

#[path = "sql_triggers/constraint.rs"]
mod constraint;
#[path = "sql_triggers/review_regressions.rs"]
mod review_regressions;

fn exec(engine: &Engine, sql: &str) -> uqa_engine::SQLResult {
    engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
}

fn strings(engine: &Engine, sql: &str, column: &str) -> Vec<String> {
    exec(engine, sql)
        .rows
        .into_iter()
        .map(|row| match row.get(column) {
            Some(Value::Str(value)) => value.clone(),
            other => panic!("expected text column `{column}`, got {other:?}"),
        })
        .collect()
}

fn install_trigger_fixture(engine: &Engine) {
    exec(
        engine,
        "CREATE TABLE items (id INTEGER PRIMARY KEY, value TEXT)",
    );
    exec(
        engine,
        "CREATE TABLE trigger_log (id BIGSERIAL PRIMARY KEY, message TEXT)",
    );
    exec(
        engine,
        "CREATE FUNCTION mutate_item() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           IF NEW.value = 'skip' THEN
             RETURN NULL;
           END IF;
           NEW.value := upper(NEW.value);
           INSERT INTO trigger_log(message) VALUES
             (TG_NAME || ':' || TG_WHEN || ':' || TG_LEVEL || ':' || TG_OP || ':' || TG_TABLE_SCHEMA || ':' || TG_TABLE_NAME || ':' || TG_NARGS || ':' || TG_ARGV[0]);
           RETURN NEW;
         END
         $$",
    );
    exec(
        engine,
        "CREATE FUNCTION log_statement() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           INSERT INTO trigger_log(message) VALUES
             (TG_NAME || ':' || TG_WHEN || ':' || TG_LEVEL || ':' || TG_OP);
           RETURN NULL;
         END
         $$",
    );
    exec(
        engine,
        "CREATE TRIGGER mutate_before BEFORE INSERT OR UPDATE OF value ON items
         FOR EACH ROW WHEN (NEW.value IS NOT NULL)
         EXECUTE FUNCTION mutate_item('argument')",
    );
    exec(
        engine,
        "CREATE TRIGGER insert_after AFTER INSERT ON items
         FOR EACH STATEMENT EXECUTE FUNCTION log_statement()",
    );
}

#[test]
fn before_row_and_after_statement_triggers_mutate_skip_and_expose_context() {
    let engine = Engine::new();
    install_trigger_fixture(&engine);

    let inserted = exec(
        &engine,
        "INSERT INTO items VALUES (1, 'first'), (2, 'skip'), (3, NULL) RETURNING id, value",
    );
    assert_eq!(inserted.affected_rows, 2);
    let rows = exec(&engine, "SELECT id, value FROM items ORDER BY id").rows;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get("value"), Some(&Value::Str("FIRST".into())));
    assert_eq!(rows[1].get("id"), Some(&Value::Int(3)));
    assert_eq!(rows[1].get("value"), Some(&Value::Null));
    assert_eq!(
        strings(
            &engine,
            "SELECT message FROM trigger_log ORDER BY id",
            "message"
        ),
        vec![
            "mutate_before:BEFORE:ROW:INSERT:public:items:1:argument",
            "insert_after:AFTER:STATEMENT:INSERT",
        ]
    );

    exec(&engine, "UPDATE items SET value = 'changed' WHERE id = 1");
    assert_eq!(
        strings(&engine, "SELECT value FROM items WHERE id = 1", "value"),
        vec!["CHANGED"]
    );
}

#[test]
fn trigger_catalog_survives_reopen_and_drop_is_transactional() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("triggers.db");
    {
        let engine = Engine::open(&path).unwrap();
        install_trigger_fixture(&engine);
        exec(&engine, "INSERT INTO items VALUES (1, 'before reopen')");
    }
    {
        let engine = Engine::open(&path).unwrap();
        exec(&engine, "INSERT INTO items VALUES (2, 'after reopen')");
        assert_eq!(
            strings(&engine, "SELECT value FROM items ORDER BY id", "value"),
            vec!["BEFORE REOPEN", "AFTER REOPEN"]
        );
        exec(&engine, "BEGIN");
        exec(&engine, "DROP TRIGGER mutate_before ON items");
        exec(&engine, "ROLLBACK");
        exec(&engine, "INSERT INTO items VALUES (3, 'after rollback')");
        assert_eq!(
            strings(&engine, "SELECT value FROM items WHERE id = 3", "value"),
            vec!["AFTER ROLLBACK"]
        );
    }
}

#[test]
fn trigger_catalog_matches_postgresql_18_shape_and_definition_helpers() {
    let engine = Engine::new();
    install_trigger_fixture(&engine);

    let rows = exec(
        &engine,
        "SELECT tgname, tgtype, tgenabled, tgisinternal, tgnargs, tgattr, tgargs
         FROM pg_catalog.pg_trigger ORDER BY tgname",
    )
    .rows;
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0].get("tgname"),
        Some(&Value::Str("insert_after".into()))
    );
    assert_eq!(rows[0].get("tgtype"), Some(&Value::Int(4)));
    assert_eq!(rows[0].get("tgenabled"), Some(&Value::Str("O".into())));
    assert_eq!(rows[0].get("tgisinternal"), Some(&Value::Bool(false)));
    assert_eq!(rows[0].get("tgnargs"), Some(&Value::Int(0)));
    assert_eq!(rows[0].get("tgattr"), Some(&Value::List(Vec::new())));
    assert_eq!(rows[0].get("tgargs"), Some(&Value::Bytes(Vec::new())));
    assert_eq!(
        rows[1].get("tgname"),
        Some(&Value::Str("mutate_before".into()))
    );
    assert_eq!(rows[1].get("tgtype"), Some(&Value::Int(23)));
    assert_eq!(rows[1].get("tgnargs"), Some(&Value::Int(1)));
    assert_eq!(
        rows[1].get("tgattr"),
        Some(&Value::List(vec![Value::Int(2)]))
    );
    assert_eq!(
        rows[1].get("tgargs"),
        Some(&Value::Bytes(b"argument\0".to_vec()))
    );

    assert_eq!(
        exec(
            &engine,
            "SELECT relhastriggers FROM pg_class WHERE relname = 'items'",
        )
        .rows[0]
            .get("relhastriggers"),
        Some(&Value::Bool(true))
    );
    assert_eq!(
        exec(
            &engine,
            "SELECT hastriggers FROM pg_tables WHERE tablename = 'items'",
        )
        .rows[0]
            .get("hastriggers"),
        Some(&Value::Bool(true))
    );
    let definition = strings(
        &engine,
        "SELECT pg_get_triggerdef(oid) AS definition FROM pg_trigger WHERE tgname = 'mutate_before'",
        "definition",
    );
    assert_eq!(definition.len(), 1);
    assert!(definition[0].contains("BEFORE INSERT OR UPDATE OF value"));
    assert!(definition[0].contains("FOR EACH ROW"));
    assert!(definition[0].contains("EXECUTE FUNCTION mutate_item('argument')"));
}

#[test]
fn trigger_dependencies_follow_table_rename_drop_and_function_cascade() {
    let engine = Engine::new();
    install_trigger_fixture(&engine);

    let error = engine
        .sql("DROP FUNCTION mutate_item()", &[])
        .expect_err("a trigger must depend on its function");
    assert!(
        matches!(error, uqa_sql::SQLError::Routine { ref sqlstate, .. } if sqlstate == "2BP01"),
        "{error}"
    );

    exec(&engine, "ALTER TABLE items RENAME TO renamed_items");
    exec(&engine, "INSERT INTO renamed_items VALUES (1, 'renamed')");
    assert_eq!(
        strings(
            &engine,
            "SELECT value FROM renamed_items WHERE id = 1",
            "value",
        ),
        vec!["RENAMED"]
    );
    let definition = strings(
        &engine,
        "SELECT pg_get_triggerdef(oid) AS definition FROM pg_trigger WHERE tgname = 'mutate_before'",
        "definition",
    );
    assert!(definition[0].contains("ON public.renamed_items"));

    exec(&engine, "DROP FUNCTION mutate_item() CASCADE");
    exec(&engine, "INSERT INTO renamed_items VALUES (2, 'plain')");
    assert_eq!(
        strings(
            &engine,
            "SELECT value FROM renamed_items WHERE id = 2",
            "value",
        ),
        vec!["plain"]
    );
    assert_eq!(
        exec(
            &engine,
            "SELECT count(*) AS n FROM pg_trigger WHERE tgname = 'mutate_before'",
        )
        .rows[0]
            .get("n"),
        Some(&Value::Int(0))
    );

    exec(&engine, "DROP TABLE renamed_items");
    assert_eq!(
        exec(&engine, "SELECT count(*) AS n FROM pg_trigger").rows[0].get("n"),
        Some(&Value::Int(0))
    );
}

#[test]
fn trigger_enable_modes_and_rename_are_durable_catalog_mutations() {
    let engine = Engine::new();
    install_trigger_fixture(&engine);

    exec(&engine, "ALTER TABLE items DISABLE TRIGGER mutate_before");
    exec(&engine, "INSERT INTO items VALUES (1, 'disabled')");
    assert_eq!(
        strings(&engine, "SELECT value FROM items WHERE id = 1", "value"),
        vec!["disabled"]
    );
    assert_eq!(
        strings(
            &engine,
            "SELECT tgenabled FROM pg_trigger WHERE tgname = 'mutate_before'",
            "tgenabled",
        ),
        vec!["D"]
    );

    exec(
        &engine,
        "ALTER TABLE items ENABLE ALWAYS TRIGGER mutate_before",
    );
    exec(
        &engine,
        "ALTER TRIGGER mutate_before ON items RENAME TO renamed_before",
    );
    exec(&engine, "INSERT INTO items VALUES (2, 'enabled')");
    assert_eq!(
        strings(&engine, "SELECT value FROM items WHERE id = 2", "value"),
        vec!["ENABLED"]
    );
    assert_eq!(
        strings(
            &engine,
            "SELECT tgenabled FROM pg_trigger WHERE tgname = 'renamed_before'",
            "tgenabled",
        ),
        vec!["A"]
    );
}

#[test]
fn trigger_creation_validates_postgresql_18_when_and_update_of_contracts() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE checked_items (
           id INTEGER PRIMARY KEY,
           generated_value INTEGER GENERATED ALWAYS AS (id + 1) STORED
         )",
    );
    exec(
        &engine,
        "CREATE FUNCTION checked_trigger() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN RETURN NEW; END
         $$",
    );
    exec(
        &engine,
        "CREATE FUNCTION skip_checked_trigger() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN RETURN NULL; END
         $$",
    );
    exec(
        &engine,
        "CREATE TRIGGER statement_when BEFORE INSERT ON checked_items
         FOR EACH STATEMENT WHEN (true) EXECUTE FUNCTION checked_trigger()",
    );

    for (sql, expected_state) in [
        (
            "CREATE TRIGGER missing_column BEFORE UPDATE OF missing ON checked_items FOR EACH ROW EXECUTE FUNCTION checked_trigger()",
            "42703",
        ),
        (
            "CREATE TRIGGER insert_old BEFORE INSERT ON checked_items FOR EACH ROW WHEN (OLD.id > 0) EXECUTE FUNCTION checked_trigger()",
            "42P17",
        ),
        (
            "CREATE TRIGGER delete_new BEFORE DELETE ON checked_items FOR EACH ROW WHEN (NEW.id > 0) EXECUTE FUNCTION checked_trigger()",
            "42P17",
        ),
        (
            "CREATE TRIGGER before_generated BEFORE INSERT ON checked_items FOR EACH ROW WHEN (NEW.generated_value > 0) EXECUTE FUNCTION checked_trigger()",
            "42P17",
        ),
        (
            "CREATE TRIGGER integer_when BEFORE INSERT ON checked_items FOR EACH ROW WHEN (NEW.id + 1) EXECUTE FUNCTION checked_trigger()",
            "42804",
        ),
        (
            "CREATE TRIGGER invalid_text_when BEFORE INSERT ON checked_items FOR EACH ROW WHEN ('not-a-boolean') EXECUTE FUNCTION checked_trigger()",
            "22P02",
        ),
    ] {
        let error = engine.sql(sql, &[]).expect_err("invalid trigger must fail");
        assert_eq!(error.sqlstate(), Some(expected_state), "{sql}: {error}");
    }
    exec(
        &engine,
        "CREATE TRIGGER false_when BEFORE INSERT ON checked_items
         FOR EACH ROW WHEN ('false') EXECUTE FUNCTION skip_checked_trigger()",
    );
    exec(&engine, "INSERT INTO checked_items(id) VALUES (1)");
    assert_eq!(
        exec(&engine, "SELECT count(*) AS n FROM checked_items").rows[0].get("n"),
        Some(&Value::Int(1))
    );
}

#[test]
fn truncate_triggers_cover_cascade_targets_in_statement_order() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE truncate_audit (id BIGSERIAL PRIMARY KEY, message TEXT)",
    );
    exec(
        &engine,
        "CREATE TABLE truncate_parent (id INTEGER PRIMARY KEY)",
    );
    exec(
        &engine,
        "CREATE TABLE truncate_child (parent_id INTEGER REFERENCES truncate_parent(id))",
    );
    exec(
        &engine,
        "CREATE FUNCTION truncate_probe() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           INSERT INTO truncate_audit(message) VALUES (TG_WHEN || ' ' || TG_TABLE_NAME);
           RETURN NULL;
         END
         $$",
    );
    for sql in [
        "CREATE TRIGGER parent_before BEFORE TRUNCATE ON truncate_parent EXECUTE FUNCTION truncate_probe()",
        "CREATE TRIGGER parent_after AFTER TRUNCATE ON truncate_parent EXECUTE FUNCTION truncate_probe()",
        "CREATE TRIGGER child_before BEFORE TRUNCATE ON truncate_child EXECUTE FUNCTION truncate_probe()",
        "CREATE TRIGGER child_after AFTER TRUNCATE ON truncate_child EXECUTE FUNCTION truncate_probe()",
    ] {
        exec(&engine, sql);
    }

    exec(&engine, "TRUNCATE truncate_parent CASCADE");
    assert_eq!(
        strings(
            &engine,
            "SELECT message FROM truncate_audit ORDER BY id",
            "message",
        ),
        vec![
            "BEFORE truncate_parent",
            "BEFORE truncate_child",
            "AFTER truncate_parent",
            "AFTER truncate_child",
        ]
    );
}

#[test]
fn partitioned_table_row_triggers_fire_on_leaf_and_surface_catalog_clones() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE partitioned_items (id INTEGER, value TEXT) PARTITION BY RANGE (id)",
    );
    exec(
        &engine,
        "CREATE TABLE partitioned_items_low PARTITION OF partitioned_items FOR VALUES FROM (0) TO (10)",
    );
    exec(
        &engine,
        "CREATE FUNCTION partition_trigger() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN NEW.value := upper(NEW.value); RETURN NEW; END
         $$",
    );
    exec(
        &engine,
        "CREATE TRIGGER partition_before BEFORE INSERT ON partitioned_items
         FOR EACH ROW EXECUTE FUNCTION partition_trigger()",
    );

    exec(&engine, "INSERT INTO partitioned_items VALUES (1, 'leaf')");
    assert_eq!(
        strings(
            &engine,
            "SELECT value FROM partitioned_items_low WHERE id = 1",
            "value",
        ),
        vec!["LEAF"]
    );
    let rows = exec(
        &engine,
        "SELECT c.relname, t.tgparentid
         FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid
         WHERE t.tgname = 'partition_before' ORDER BY c.relname",
    )
    .rows;
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0].get("relname"),
        Some(&Value::Str("partitioned_items".into()))
    );
    assert_eq!(rows[0].get("tgparentid"), Some(&Value::Int(0)));
    assert_eq!(
        rows[1].get("relname"),
        Some(&Value::Str("partitioned_items_low".into()))
    );
    assert!(matches!(rows[1].get("tgparentid"), Some(Value::Int(value)) if *value != 0));
}

fn install_action_trigger_fixture(engine: &Engine) {
    exec(
        engine,
        "CREATE TABLE action_items (id INTEGER PRIMARY KEY, value TEXT)",
    );
    exec(
        engine,
        "CREATE TABLE action_source (id INTEGER PRIMARY KEY, value TEXT)",
    );
    exec(
        engine,
        "CREATE TABLE action_audit (id BIGSERIAL PRIMARY KEY, message TEXT)",
    );
    exec(
        engine,
        "INSERT INTO action_items VALUES (1, 'seed'), (3, 'delete')",
    );
    exec(
        engine,
        "INSERT INTO action_source VALUES (1, 'updated'), (2, 'inserted')",
    );
    exec(
        engine,
        "CREATE FUNCTION action_trigger() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           INSERT INTO action_audit(message) VALUES (TG_WHEN || ' ' || TG_LEVEL || ' ' || TG_OP);
           IF TG_LEVEL = 'ROW' AND TG_OP IN ('INSERT', 'UPDATE') THEN
             NEW.value := upper(NEW.value);
           END IF;
           IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
           RETURN NEW;
         END
         $$",
    );
    for sql in [
        "CREATE TRIGGER bi_s BEFORE INSERT ON action_items FOR EACH STATEMENT EXECUTE FUNCTION action_trigger()",
        "CREATE TRIGGER bu_s BEFORE UPDATE ON action_items FOR EACH STATEMENT EXECUTE FUNCTION action_trigger()",
        "CREATE TRIGGER bd_s BEFORE DELETE ON action_items FOR EACH STATEMENT EXECUTE FUNCTION action_trigger()",
        "CREATE TRIGGER bi_r BEFORE INSERT ON action_items FOR EACH ROW EXECUTE FUNCTION action_trigger()",
        "CREATE TRIGGER bu_r BEFORE UPDATE ON action_items FOR EACH ROW EXECUTE FUNCTION action_trigger()",
        "CREATE TRIGGER bd_r BEFORE DELETE ON action_items FOR EACH ROW EXECUTE FUNCTION action_trigger()",
        "CREATE TRIGGER ai_r AFTER INSERT ON action_items FOR EACH ROW EXECUTE FUNCTION action_trigger()",
        "CREATE TRIGGER au_r AFTER UPDATE ON action_items FOR EACH ROW EXECUTE FUNCTION action_trigger()",
        "CREATE TRIGGER ad_r AFTER DELETE ON action_items FOR EACH ROW EXECUTE FUNCTION action_trigger()",
        "CREATE TRIGGER ai_s AFTER INSERT ON action_items FOR EACH STATEMENT EXECUTE FUNCTION action_trigger()",
        "CREATE TRIGGER au_s AFTER UPDATE ON action_items FOR EACH STATEMENT EXECUTE FUNCTION action_trigger()",
        "CREATE TRIGGER ad_s AFTER DELETE ON action_items FOR EACH STATEMENT EXECUTE FUNCTION action_trigger()",
    ] {
        exec(engine, sql);
    }
}

#[test]
fn on_conflict_and_merge_fire_action_triggers_in_postgresql_order() {
    let engine = Engine::new();
    install_action_trigger_fixture(&engine);

    exec(
        &engine,
        "INSERT INTO action_items VALUES (1, 'conflict')
         ON CONFLICT (id) DO UPDATE SET value = excluded.value",
    );
    assert_eq!(
        strings(
            &engine,
            "SELECT message FROM action_audit ORDER BY id",
            "message",
        ),
        vec![
            "BEFORE STATEMENT INSERT",
            "BEFORE STATEMENT UPDATE",
            "BEFORE ROW INSERT",
            "BEFORE ROW UPDATE",
            "AFTER ROW UPDATE",
            "AFTER STATEMENT UPDATE",
            "AFTER STATEMENT INSERT",
        ]
    );
    assert_eq!(
        strings(
            &engine,
            "SELECT value FROM action_items WHERE id = 1",
            "value"
        ),
        vec!["CONFLICT"]
    );

    exec(&engine, "TRUNCATE action_audit RESTART IDENTITY");
    exec(
        &engine,
        "MERGE INTO action_items AS target USING action_source AS source
         ON target.id = source.id
         WHEN MATCHED THEN UPDATE SET value = source.value
         WHEN NOT MATCHED THEN INSERT (id, value) VALUES (source.id, source.value)
         WHEN NOT MATCHED BY SOURCE THEN DELETE",
    );
    let messages = strings(
        &engine,
        "SELECT message FROM action_audit ORDER BY id",
        "message",
    );
    assert_eq!(
        &messages[..3],
        [
            "BEFORE STATEMENT INSERT",
            "BEFORE STATEMENT UPDATE",
            "BEFORE STATEMENT DELETE",
        ]
    );
    assert_eq!(
        &messages[messages.len() - 3..],
        [
            "AFTER STATEMENT DELETE",
            "AFTER STATEMENT UPDATE",
            "AFTER STATEMENT INSERT",
        ]
    );
    assert!(messages.contains(&"BEFORE ROW UPDATE".to_string()));
    assert!(messages.contains(&"BEFORE ROW INSERT".to_string()));
    assert!(messages.contains(&"BEFORE ROW DELETE".to_string()));
    assert_eq!(
        strings(
            &engine,
            "SELECT value FROM action_items ORDER BY id",
            "value"
        ),
        vec!["UPDATED", "INSERTED"]
    );
}

#[test]
fn trigger_column_dependencies_follow_rename_and_drop_cascade() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE column_items (id INTEGER PRIMARY KEY, value INTEGER)",
    );
    exec(
        &engine,
        "CREATE FUNCTION column_trigger() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN RETURN NEW; END
         $$",
    );
    exec(
        &engine,
        "CREATE TRIGGER value_trigger BEFORE UPDATE OF value ON column_items
         FOR EACH ROW WHEN (NEW.value > 0) EXECUTE FUNCTION column_trigger()",
    );

    exec(
        &engine,
        "ALTER TABLE column_items RENAME COLUMN value TO renamed_value",
    );
    let definition = strings(
        &engine,
        "SELECT pg_get_triggerdef(oid) AS definition FROM pg_trigger WHERE tgname = 'value_trigger'",
        "definition",
    );
    assert!(
        definition[0].contains("UPDATE OF renamed_value"),
        "{definition:?}"
    );
    assert!(
        definition[0].contains("new.renamed_value"),
        "{definition:?}"
    );

    let error = engine
        .sql("ALTER TABLE column_items DROP COLUMN renamed_value", &[])
        .expect_err("trigger column dependency must restrict DROP COLUMN");
    assert_eq!(error.sqlstate(), Some("2BP01"));
    exec(
        &engine,
        "ALTER TABLE column_items DROP COLUMN renamed_value CASCADE",
    );
    assert_eq!(
        exec(&engine, "SELECT count(*) AS n FROM pg_trigger").rows[0].get("n"),
        Some(&Value::Int(0))
    );
}

#[test]
fn before_insert_trigger_primary_key_change_updates_physical_identity() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE identity_items (id INTEGER PRIMARY KEY, value TEXT)",
    );
    exec(
        &engine,
        "CREATE FUNCTION move_identity() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN NEW.id := NEW.id + 100; RETURN NEW; END
         $$",
    );
    exec(
        &engine,
        "CREATE TRIGGER move_before BEFORE INSERT ON identity_items
         FOR EACH ROW EXECUTE FUNCTION move_identity()",
    );

    let inserted = exec(
        &engine,
        "INSERT INTO identity_items VALUES (1, 'moved') RETURNING id",
    );
    assert_eq!(inserted.rows[0].get("id"), Some(&Value::Int(101)));
    assert_eq!(
        exec(&engine, "SELECT id FROM identity_items WHERE id = 101").rows[0].get("id"),
        Some(&Value::Int(101))
    );
    assert!(exec(&engine, "SELECT id FROM identity_items WHERE id = 1")
        .rows
        .is_empty());
}

fn install_timing_trigger_fixture(engine: &Engine) {
    exec(
        engine,
        "CREATE TABLE timing_items (
           id INTEGER PRIMARY KEY,
           value INTEGER,
           generated_value INTEGER GENERATED ALWAYS AS (value * 10) STORED
         )",
    );
    exec(
        engine,
        "CREATE TABLE timing_log (id BIGSERIAL PRIMARY KEY, message TEXT)",
    );
    exec(
        engine,
        "CREATE FUNCTION timing_row_trigger() RETURNS trigger LANGUAGE plpgsql AS $$
         DECLARE visible_rows INTEGER;
         BEGIN
           IF TG_WHEN = 'BEFORE' THEN
             INSERT INTO timing_log(message) VALUES
               (TG_NAME || ':' || TG_WHEN || ':' || TG_OP || ':g=' || coalesce(NEW.generated_value::text, 'NULL'));
           ELSE
             SELECT count(*) INTO visible_rows FROM timing_items;
             INSERT INTO timing_log(message) VALUES
               (TG_NAME || ':' || TG_WHEN || ':' || TG_OP || ':rows=' || visible_rows::text);
           END IF;
           RETURN NEW;
         END
         $$",
    );
    exec(
        engine,
        "CREATE FUNCTION timing_statement_trigger() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           INSERT INTO timing_log(message) VALUES (TG_NAME || ':' || TG_WHEN || ':' || TG_OP);
           RETURN NULL;
         END
         $$",
    );
    exec(
        engine,
        "CREATE FUNCTION timing_visible_count() RETURNS INTEGER LANGUAGE SQL VOLATILE
         AS 'SELECT count(*)::integer FROM timing_items'",
    );
    for sql in [
        "CREATE TRIGGER z_before BEFORE INSERT ON timing_items FOR EACH ROW EXECUTE FUNCTION timing_row_trigger()",
        "CREATE TRIGGER a_before BEFORE INSERT ON timing_items FOR EACH ROW EXECUTE FUNCTION timing_row_trigger()",
        "CREATE TRIGGER after_insert AFTER INSERT ON timing_items FOR EACH ROW EXECUTE FUNCTION timing_row_trigger()",
        "CREATE TRIGGER after_when AFTER INSERT ON timing_items FOR EACH ROW WHEN (timing_visible_count() = NEW.id) EXECUTE FUNCTION timing_row_trigger()",
        "CREATE TRIGGER after_zero_update AFTER UPDATE ON timing_items FOR EACH STATEMENT EXECUTE FUNCTION timing_statement_trigger()",
    ] {
        exec(engine, sql);
    }
}

#[test]
fn trigger_timing_matches_postgresql_deferred_after_rows_and_generated_images() {
    let engine = Engine::new();
    install_timing_trigger_fixture(&engine);

    exec(
        &engine,
        "INSERT INTO timing_items(id, value) VALUES (1, 1), (2, 2)",
    );
    exec(
        &engine,
        "UPDATE timing_items SET value = value + 1 WHERE id = 999",
    );
    assert_eq!(
        strings(
            &engine,
            "SELECT message FROM timing_log ORDER BY id",
            "message",
        ),
        vec![
            "a_before:BEFORE:INSERT:g=NULL",
            "z_before:BEFORE:INSERT:g=NULL",
            "a_before:BEFORE:INSERT:g=NULL",
            "z_before:BEFORE:INSERT:g=NULL",
            "after_insert:AFTER:INSERT:rows=2",
            "after_when:AFTER:INSERT:rows=2",
            "after_insert:AFTER:INSERT:rows=2",
            "after_when:AFTER:INSERT:rows=2",
            "after_zero_update:AFTER:UPDATE",
        ]
    );

    exec(&engine, "TRUNCATE timing_items");
    exec(&engine, "TRUNCATE timing_log RESTART IDENTITY");
    exec(
        &engine,
        "INSERT INTO timing_items(id, value)
         SELECT source.id, source.value
         FROM (VALUES (1, 1), (2, 2)) AS source(id, value)",
    );
    assert_eq!(
        exec(
            &engine,
            "SELECT count(*) AS n FROM timing_log WHERE message LIKE 'after_when:%'",
        )
        .rows[0]
            .get("n"),
        Some(&Value::Int(2))
    );

    let error = engine
        .sql("SELECT timing_row_trigger()", &[])
        .expect_err("trigger functions must reject direct calls");
    assert_eq!(error.sqlstate(), Some("0A000"), "{error}");
    assert!(error
        .to_string()
        .contains("trigger functions can only be called as triggers"));
}
