//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn temporary_trigger_survives_unrelated_catalog_reload() {
    let directory = TempDir::new().unwrap();
    let engine = Engine::open(&directory.path().join("temporary-trigger.db")).unwrap();
    exec(&engine, "CREATE TEMP TABLE temporary_items (value TEXT)");
    exec(&engine, "CREATE TEMP TABLE temporary_audit (message TEXT)");
    exec(
        &engine,
        "CREATE FUNCTION temporary_trigger_probe() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           NEW.value := upper(NEW.value);
           INSERT INTO temporary_audit(message) VALUES (NEW.value);
           RETURN NEW;
         END
         $$",
    );
    exec(
        &engine,
        "CREATE TRIGGER temporary_before BEFORE INSERT ON temporary_items
         FOR EACH ROW EXECUTE FUNCTION temporary_trigger_probe()",
    );

    exec(&engine, "BEGIN");
    exec(
        &engine,
        "CREATE FUNCTION rolled_back_catalog_change() RETURNS integer RETURN 1",
    );
    exec(&engine, "ROLLBACK");
    exec(&engine, "INSERT INTO temporary_items VALUES ('retained')");

    assert_eq!(
        strings(&engine, "SELECT value FROM temporary_items", "value"),
        vec!["RETAINED"]
    );
    assert_eq!(
        strings(&engine, "SELECT message FROM temporary_audit", "message"),
        vec!["RETAINED"]
    );
}

#[test]
fn pg_get_triggerdef_honors_compact_pretty_and_null_arguments() {
    let engine = Engine::new();
    install_trigger_fixture(&engine);

    let row = &exec(
        &engine,
        "SELECT pg_get_triggerdef(oid, false) AS compact,
                pg_get_triggerdef(oid, true) AS pretty,
                pg_get_triggerdef(oid, NULL) AS null_definition
         FROM pg_trigger WHERE tgname = 'mutate_before'",
    )
    .rows[0];
    assert_eq!(
        row.get("compact"),
        Some(&Value::Str(
            "CREATE TRIGGER mutate_before BEFORE INSERT OR UPDATE OF value ON public.items FOR EACH ROW WHEN ((new.value IS NOT NULL)) EXECUTE FUNCTION mutate_item('argument')".into(),
        ))
    );
    assert_eq!(
        row.get("pretty"),
        Some(&Value::Str(
            "CREATE TRIGGER mutate_before BEFORE INSERT OR UPDATE OF value ON items FOR EACH ROW WHEN (new.value IS NOT NULL) EXECUTE FUNCTION mutate_item('argument')".into(),
        ))
    );
    assert_eq!(row.get("null_definition"), Some(&Value::Null));

    exec(&engine, "SET search_path TO pg_catalog");
    let qualified = strings(
        &engine,
        "SELECT pg_get_triggerdef(oid, true) AS definition FROM pg_trigger WHERE tgname = 'mutate_before'",
        "definition",
    );
    assert!(qualified[0].contains(" ON public.items "));
    assert!(qualified[0].contains(" FUNCTION public.mutate_item("));
}

#[test]
fn virtual_generated_columns_are_null_in_every_row_trigger_image() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE virtual_items (
           id INTEGER PRIMARY KEY,
           generated_value INTEGER GENERATED ALWAYS AS (id + 1) VIRTUAL
         )",
    );
    exec(
        &engine,
        "CREATE TABLE virtual_trigger_audit (
           id BIGSERIAL PRIMARY KEY,
           phase TEXT,
           event TEXT,
           old_value INTEGER,
           new_value INTEGER
         )",
    );
    exec(
        &engine,
        "CREATE FUNCTION virtual_trigger_probe() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           IF TG_OP = 'INSERT' THEN
             INSERT INTO virtual_trigger_audit(phase, event, old_value, new_value)
             VALUES (TG_WHEN, TG_OP, NULL, NEW.generated_value);
           ELSE
             INSERT INTO virtual_trigger_audit(phase, event, old_value, new_value)
             VALUES (TG_WHEN, TG_OP, OLD.generated_value, NEW.generated_value);
           END IF;
           RETURN NEW;
         END
         $$",
    );
    for sql in [
        "CREATE TRIGGER virtual_before BEFORE INSERT OR UPDATE ON virtual_items FOR EACH ROW EXECUTE FUNCTION virtual_trigger_probe()",
        "CREATE TRIGGER virtual_after AFTER INSERT OR UPDATE ON virtual_items FOR EACH ROW EXECUTE FUNCTION virtual_trigger_probe()",
    ] {
        exec(&engine, sql);
    }

    exec(&engine, "INSERT INTO virtual_items(id) VALUES (10)");
    exec(&engine, "UPDATE virtual_items SET id = 20 WHERE id = 10");
    assert_eq!(
        exec(
            &engine,
            "SELECT count(*) AS n FROM virtual_trigger_audit
             WHERE old_value IS NOT NULL OR new_value IS NOT NULL",
        )
        .rows[0]
            .get("n"),
        Some(&Value::Int(0))
    );
    assert_eq!(
        exec(&engine, "SELECT generated_value FROM virtual_items").rows[0].get("generated_value"),
        Some(&Value::Int(21))
    );
}

#[test]
fn suppressed_merge_action_does_not_consume_target_identity() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE merge_trigger_items (id INTEGER PRIMARY KEY, value TEXT)",
    );
    exec(
        &engine,
        "CREATE TABLE merge_trigger_state (n INTEGER NOT NULL)",
    );
    exec(
        &engine,
        "INSERT INTO merge_trigger_items VALUES (1, 'original'), (2, 'delete')",
    );
    exec(&engine, "INSERT INTO merge_trigger_state VALUES (0)");
    exec(
        &engine,
        "CREATE FUNCTION suppress_first_merge_action() RETURNS trigger LANGUAGE plpgsql AS $$
         DECLARE current_n INTEGER;
         BEGIN
           UPDATE merge_trigger_state SET n = n + 1 RETURNING n INTO current_n;
           IF current_n = 1 THEN RETURN NULL; END IF;
           IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
           RETURN NEW;
         END
         $$",
    );
    exec(
        &engine,
        "CREATE TRIGGER suppress_merge BEFORE UPDATE OR DELETE ON merge_trigger_items
         FOR EACH ROW EXECUTE FUNCTION suppress_first_merge_action()",
    );

    let updated = exec(
        &engine,
        "MERGE INTO merge_trigger_items AS target
         USING (VALUES (1, 'first'), (1, 'second')) AS source(id, value)
         ON target.id = source.id
         WHEN MATCHED THEN UPDATE SET value = source.value",
    );
    assert_eq!(updated.affected_rows, 1);
    assert_eq!(
        strings(
            &engine,
            "SELECT value FROM merge_trigger_items WHERE id = 1",
            "value",
        ),
        vec!["second"]
    );

    exec(&engine, "UPDATE merge_trigger_state SET n = 0");
    let deleted = exec(
        &engine,
        "MERGE INTO merge_trigger_items AS target
         USING (VALUES (2), (2)) AS source(id)
         ON target.id = source.id
         WHEN MATCHED THEN DELETE",
    );
    assert_eq!(deleted.affected_rows, 1);
    assert_eq!(
        exec(
            &engine,
            "SELECT count(*) AS n FROM merge_trigger_items WHERE id = 2",
        )
        .rows[0]
            .get("n"),
        Some(&Value::Int(0))
    );
}

fn install_referential_delete_trigger_fixture(engine: &Engine) {
    exec(
        engine,
        "CREATE TABLE cascade_trigger_audit (id BIGSERIAL PRIMARY KEY, message TEXT)",
    );
    exec(
        engine,
        "CREATE TABLE cascade_trigger_parent (id INTEGER PRIMARY KEY)",
    );
    exec(
        engine,
        "CREATE TABLE cascade_trigger_delete (
           id INTEGER PRIMARY KEY,
           parent_id INTEGER REFERENCES cascade_trigger_parent(id) ON DELETE CASCADE
         )",
    );
    exec(
        engine,
        "CREATE TABLE cascade_trigger_grandchild (
           id INTEGER PRIMARY KEY,
           child_id INTEGER REFERENCES cascade_trigger_delete(id) ON DELETE CASCADE
         )",
    );
    exec(
        engine,
        "CREATE TABLE cascade_trigger_null (
           id INTEGER PRIMARY KEY,
           parent_id INTEGER REFERENCES cascade_trigger_parent(id) ON DELETE SET NULL,
           marker TEXT
         )",
    );
    exec(
        engine,
        "CREATE FUNCTION cascade_delete_probe() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           INSERT INTO cascade_trigger_audit(message)
           VALUES (TG_TABLE_NAME || ':' || TG_WHEN || ':DELETE:' || OLD.id::text);
           IF TG_WHEN = 'BEFORE' AND TG_TABLE_NAME = 'cascade_trigger_delete' AND OLD.id = 11 THEN
             RETURN NULL;
           END IF;
           RETURN OLD;
         END
         $$",
    );
    exec(
        engine,
        "CREATE FUNCTION cascade_update_probe() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           INSERT INTO cascade_trigger_audit(message)
           VALUES (TG_TABLE_NAME || ':' || TG_WHEN || ':UPDATE:' || OLD.id::text);
           IF TG_WHEN = 'BEFORE' THEN NEW.marker := upper(NEW.marker); END IF;
           RETURN NEW;
         END
         $$",
    );
    exec(
        engine,
        "CREATE FUNCTION cascade_statement_probe() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           INSERT INTO cascade_trigger_audit(message)
           VALUES (TG_TABLE_NAME || ':' || TG_WHEN || ':STATEMENT:' || TG_OP);
           RETURN NULL;
         END
         $$",
    );
    for sql in [
        "CREATE TRIGGER delete_statement_before BEFORE DELETE ON cascade_trigger_delete FOR EACH STATEMENT EXECUTE FUNCTION cascade_statement_probe()",
        "CREATE TRIGGER delete_before BEFORE DELETE ON cascade_trigger_delete FOR EACH ROW EXECUTE FUNCTION cascade_delete_probe()",
        "CREATE TRIGGER delete_after AFTER DELETE ON cascade_trigger_delete FOR EACH ROW EXECUTE FUNCTION cascade_delete_probe()",
        "CREATE TRIGGER delete_statement_after AFTER DELETE ON cascade_trigger_delete FOR EACH STATEMENT EXECUTE FUNCTION cascade_statement_probe()",
        "CREATE TRIGGER grandchild_statement_before BEFORE DELETE ON cascade_trigger_grandchild FOR EACH STATEMENT EXECUTE FUNCTION cascade_statement_probe()",
        "CREATE TRIGGER grandchild_before BEFORE DELETE ON cascade_trigger_grandchild FOR EACH ROW EXECUTE FUNCTION cascade_delete_probe()",
        "CREATE TRIGGER grandchild_after AFTER DELETE ON cascade_trigger_grandchild FOR EACH ROW EXECUTE FUNCTION cascade_delete_probe()",
        "CREATE TRIGGER grandchild_statement_after AFTER DELETE ON cascade_trigger_grandchild FOR EACH STATEMENT EXECUTE FUNCTION cascade_statement_probe()",
        "CREATE TRIGGER null_statement_before BEFORE UPDATE ON cascade_trigger_null FOR EACH STATEMENT EXECUTE FUNCTION cascade_statement_probe()",
        "CREATE TRIGGER null_before BEFORE UPDATE ON cascade_trigger_null FOR EACH ROW EXECUTE FUNCTION cascade_update_probe()",
        "CREATE TRIGGER null_after AFTER UPDATE ON cascade_trigger_null FOR EACH ROW EXECUTE FUNCTION cascade_update_probe()",
        "CREATE TRIGGER null_statement_after AFTER UPDATE ON cascade_trigger_null FOR EACH STATEMENT EXECUTE FUNCTION cascade_statement_probe()",
    ] {
        exec(engine, sql);
    }
    exec(engine, "INSERT INTO cascade_trigger_parent VALUES (1), (2)");
    exec(
        engine,
        "INSERT INTO cascade_trigger_delete VALUES (10, 1), (11, 1), (20, 2)",
    );
    exec(
        engine,
        "INSERT INTO cascade_trigger_grandchild VALUES (100, 10), (110, 11)",
    );
    exec(
        engine,
        "INSERT INTO cascade_trigger_null VALUES (30, 1, 'changed')",
    );
}

#[test]
fn referential_delete_and_rewrite_actions_fire_recursive_row_triggers() {
    let engine = Engine::new();
    install_referential_delete_trigger_fixture(&engine);

    exec(&engine, "DELETE FROM cascade_trigger_parent WHERE id = 1");
    assert_eq!(
        exec(
            &engine,
            "SELECT count(*) AS n FROM cascade_trigger_delete WHERE id = 10",
        )
        .rows[0]
            .get("n"),
        Some(&Value::Int(0))
    );
    assert_eq!(
        exec(
            &engine,
            "SELECT count(*) AS n FROM cascade_trigger_delete WHERE id = 11",
        )
        .rows[0]
            .get("n"),
        Some(&Value::Int(1))
    );
    assert_eq!(
        strings(
            &engine,
            "SELECT marker FROM cascade_trigger_null WHERE id = 30",
            "marker",
        ),
        vec!["CHANGED"]
    );
    assert_eq!(
        exec(
            &engine,
            "SELECT parent_id FROM cascade_trigger_null WHERE id = 30",
        )
        .rows[0]
            .get("parent_id"),
        Some(&Value::Null)
    );
    for expected in [
        "cascade_trigger_delete:BEFORE:DELETE:10",
        "cascade_trigger_delete:AFTER:DELETE:10",
        "cascade_trigger_delete:BEFORE:DELETE:11",
        "cascade_trigger_grandchild:BEFORE:DELETE:100",
        "cascade_trigger_grandchild:AFTER:DELETE:100",
        "cascade_trigger_null:BEFORE:UPDATE:30",
        "cascade_trigger_null:AFTER:UPDATE:30",
        "cascade_trigger_delete:BEFORE:STATEMENT:DELETE",
        "cascade_trigger_delete:AFTER:STATEMENT:DELETE",
        "cascade_trigger_grandchild:BEFORE:STATEMENT:DELETE",
        "cascade_trigger_grandchild:AFTER:STATEMENT:DELETE",
        "cascade_trigger_null:BEFORE:STATEMENT:UPDATE",
        "cascade_trigger_null:AFTER:STATEMENT:UPDATE",
    ] {
        assert_eq!(
            exec(
                &engine,
                &format!(
                    "SELECT count(*) AS n FROM cascade_trigger_audit WHERE message = '{expected}'"
                ),
            )
            .rows[0]
                .get("n"),
            Some(&Value::Int(1)),
            "missing {expected}"
        );
    }

    exec(&engine, "TRUNCATE cascade_trigger_audit");
    exec(
        &engine,
        "MERGE INTO cascade_trigger_parent AS target
         USING (VALUES (2)) AS source(id)
         ON target.id = source.id
         WHEN MATCHED THEN DELETE",
    );
    for expected in [
        "cascade_trigger_delete:BEFORE:DELETE:20",
        "cascade_trigger_delete:AFTER:DELETE:20",
    ] {
        assert_eq!(
            exec(
                &engine,
                &format!(
                    "SELECT count(*) AS n FROM cascade_trigger_audit WHERE message = '{expected}'"
                ),
            )
            .rows[0]
                .get("n"),
            Some(&Value::Int(1)),
            "missing {expected}"
        );
    }
}

fn install_referential_update_trigger_fixture(engine: &Engine) {
    exec(
        engine,
        "CREATE TABLE update_cascade_parent (
           id INTEGER PRIMARY KEY,
           key_value TEXT UNIQUE
         )",
    );
    exec(
        engine,
        "CREATE TABLE update_cascade_child (
           id INTEGER PRIMARY KEY,
           parent_id INTEGER REFERENCES update_cascade_parent(id) ON UPDATE CASCADE,
           marker TEXT
         )",
    );
    exec(
        engine,
        "CREATE TABLE update_cascade_audit (id BIGSERIAL PRIMARY KEY, message TEXT)",
    );
    exec(
        engine,
        "CREATE FUNCTION update_cascade_row_probe() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           INSERT INTO update_cascade_audit(message)
           VALUES (TG_WHEN || ':ROW:' || OLD.parent_id::text || ':' || NEW.parent_id::text);
           IF TG_WHEN = 'BEFORE' THEN NEW.marker := upper(NEW.marker); END IF;
           RETURN NEW;
         END
         $$",
    );
    exec(
        engine,
        "CREATE FUNCTION update_cascade_statement_probe() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           INSERT INTO update_cascade_audit(message) VALUES (TG_WHEN || ':STATEMENT');
           RETURN NULL;
         END
         $$",
    );
    for sql in [
        "CREATE TRIGGER child_statement_before BEFORE UPDATE ON update_cascade_child FOR EACH STATEMENT EXECUTE FUNCTION update_cascade_statement_probe()",
        "CREATE TRIGGER child_row_before BEFORE UPDATE ON update_cascade_child FOR EACH ROW EXECUTE FUNCTION update_cascade_row_probe()",
        "CREATE TRIGGER child_row_after AFTER UPDATE ON update_cascade_child FOR EACH ROW EXECUTE FUNCTION update_cascade_row_probe()",
        "CREATE TRIGGER child_statement_after AFTER UPDATE ON update_cascade_child FOR EACH STATEMENT EXECUTE FUNCTION update_cascade_statement_probe()",
    ] {
        exec(engine, sql);
    }
    exec(
        engine,
        "INSERT INTO update_cascade_parent VALUES (1, 'root')",
    );
    exec(
        engine,
        "INSERT INTO update_cascade_child VALUES (10, 1, 'changed')",
    );
}

#[test]
fn referential_update_triggers_cover_update_merge_and_on_conflict() {
    let engine = Engine::new();
    install_referential_update_trigger_fixture(&engine);

    for (sql, new_parent_id) in [
        ("UPDATE update_cascade_parent SET id = 2 WHERE id = 1", 2),
        (
            "MERGE INTO update_cascade_parent AS target
             USING (VALUES ('root', 3)) AS source(key_value, new_id)
             ON target.key_value = source.key_value
             WHEN MATCHED THEN UPDATE SET id = source.new_id",
            3,
        ),
        (
            "INSERT INTO update_cascade_parent(id, key_value) VALUES (999, 'root')
             ON CONFLICT (key_value) DO UPDATE SET id = 4",
            4,
        ),
    ] {
        exec(&engine, "TRUNCATE update_cascade_audit");
        exec(&engine, sql);
        assert_eq!(
            exec(
                &engine,
                "SELECT parent_id FROM update_cascade_child WHERE id = 10",
            )
            .rows[0]
                .get("parent_id"),
            Some(&Value::Int(new_parent_id))
        );
        assert_eq!(
            strings(
                &engine,
                "SELECT message FROM update_cascade_audit ORDER BY id",
                "message",
            ),
            vec![
                "BEFORE:STATEMENT".to_string(),
                format!("BEFORE:ROW:{}:{new_parent_id}", new_parent_id - 1),
                format!("AFTER:ROW:{}:{new_parent_id}", new_parent_id - 1),
                "AFTER:STATEMENT".to_string(),
            ]
        );
    }
    assert_eq!(
        strings(
            &engine,
            "SELECT marker FROM update_cascade_child WHERE id = 10",
            "marker",
        ),
        vec!["CHANGED"]
    );

    exec(&engine, "DELETE FROM update_cascade_child");
    exec(&engine, "TRUNCATE update_cascade_audit");
    exec(
        &engine,
        "UPDATE update_cascade_parent SET id = 5 WHERE id = 4",
    );
    assert_eq!(
        strings(
            &engine,
            "SELECT message FROM update_cascade_audit ORDER BY id",
            "message",
        ),
        vec!["BEFORE:STATEMENT", "AFTER:STATEMENT"]
    );
}
