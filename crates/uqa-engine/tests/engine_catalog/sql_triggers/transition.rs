//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 trigger transition-relation coverage.

use tempfile::TempDir;
use uqa_core::Value;
use uqa_engine::Engine;

use super::{exec, strings};

fn install_transition_fixture(engine: &Engine) {
    exec(
        engine,
        "CREATE TABLE transition_items (id INTEGER PRIMARY KEY, value INTEGER, generated_value INTEGER GENERATED ALWAYS AS (value * 10) STORED)",
    );
    exec(
        engine,
        "CREATE TABLE transition_log (id BIGSERIAL PRIMARY KEY, message TEXT)",
    );
    exec(
        engine,
        "CREATE FUNCTION transition_mutate_row() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN NEW.value := NEW.value + 1; RETURN NEW; END $$",
    );
    exec(
        engine,
        "CREATE FUNCTION transition_probe() RETURNS trigger LANGUAGE plpgsql AS $$
         DECLARE old_count BIGINT := 0; new_count BIGINT := 0; old_sum BIGINT := 0; new_sum BIGINT := 0;
         BEGIN
           IF TG_OP IN ('UPDATE', 'DELETE') THEN
             SELECT count(*), coalesce(sum(value), 0) INTO old_count, old_sum FROM old_rows;
           END IF;
           IF TG_OP IN ('INSERT', 'UPDATE') THEN
             SELECT count(*), coalesce(sum(value), 0) INTO new_count, new_sum FROM new_rows;
           END IF;
           INSERT INTO transition_log(message) VALUES (TG_NAME || ':' || TG_OP || ':' || old_count::text || ':' || new_count::text || ':' || old_sum::text || ':' || new_sum::text);
           RETURN NULL;
         END $$",
    );
    exec(
        engine,
        "CREATE TRIGGER transition_mutate BEFORE INSERT OR UPDATE ON transition_items FOR EACH ROW EXECUTE FUNCTION transition_mutate_row()",
    );
}

#[test]
fn transition_relation_validation_matches_postgresql_18_sqlstates() {
    let engine = Engine::new();
    install_transition_fixture(&engine);
    for (sql, expected_state) in [
        (
            "CREATE TRIGGER bad_before BEFORE INSERT ON transition_items REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe()",
            "42P17",
        ),
        (
            "CREATE TRIGGER bad_events AFTER INSERT OR UPDATE ON transition_items REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe()",
            "0A000",
        ),
        (
            "CREATE TRIGGER bad_columns AFTER UPDATE OF value ON transition_items REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe()",
            "0A000",
        ),
        (
            "CREATE TRIGGER bad_insert_old AFTER INSERT ON transition_items REFERENCING OLD TABLE AS old_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe()",
            "42P17",
        ),
        (
            "CREATE TRIGGER bad_delete_new AFTER DELETE ON transition_items REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe()",
            "42P17",
        ),
        (
            "CREATE TRIGGER bad_same_name AFTER UPDATE ON transition_items REFERENCING OLD TABLE AS changed_rows NEW TABLE AS changed_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe()",
            "42P17",
        ),
        (
            "CREATE TRIGGER bad_old_twice AFTER UPDATE ON transition_items REFERENCING OLD TABLE AS old_rows OLD TABLE AS older_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe()",
            "42P17",
        ),
        (
            "CREATE TRIGGER bad_row_name AFTER INSERT ON transition_items REFERENCING NEW ROW AS new_row FOR EACH ROW EXECUTE FUNCTION transition_probe()",
            "0A000",
        ),
        (
            "CREATE TRIGGER bad_truncate AFTER TRUNCATE ON transition_items REFERENCING OLD TABLE AS old_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe()",
            "0A000",
        ),
    ] {
        let error = engine.sql(sql, &[]).expect_err("invalid transition relation must fail");
        assert_eq!(error.sqlstate(), Some(expected_state), "{sql}: {error}");
    }
}

#[test]
fn transition_relation_catalog_names_and_definition_survive_reopen() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("transition.db");
    {
        let engine = Engine::open(&path).unwrap();
        install_transition_fixture(&engine);
        exec(
            &engine,
            "CREATE TRIGGER transition_update AFTER UPDATE ON transition_items REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe()",
        );
    }
    let engine = Engine::open(&path).unwrap();
    let row = exec(
        &engine,
        "SELECT tgoldtable, tgnewtable, pg_get_triggerdef(oid, true) AS definition FROM pg_trigger WHERE tgname = 'transition_update'",
    )
    .rows
    .into_iter()
    .next()
    .expect("transition trigger catalog row");
    assert_eq!(row.get("tgoldtable"), Some(&Value::Str("old_rows".into())));
    assert_eq!(row.get("tgnewtable"), Some(&Value::Str("new_rows".into())));
    assert_eq!(
        row.get("definition"),
        Some(&Value::Str(
            "CREATE TRIGGER transition_update AFTER UPDATE ON transition_items REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe()".into()
        ))
    );
}

#[test]
fn transition_relations_execute_setwise_for_row_statement_and_zero_row_updates() {
    let engine = Engine::new();
    install_transition_fixture(&engine);
    exec(
        &engine,
        "CREATE TRIGGER transition_insert_statement AFTER INSERT ON transition_items REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe()",
    );
    exec(
        &engine,
        "CREATE TRIGGER transition_insert_row AFTER INSERT ON transition_items REFERENCING NEW TABLE AS new_rows FOR EACH ROW EXECUTE FUNCTION transition_probe()",
    );
    exec(
        &engine,
        "CREATE TRIGGER transition_update_statement AFTER UPDATE ON transition_items REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe()",
    );
    exec(
        &engine,
        "CREATE TRIGGER transition_delete_statement AFTER DELETE ON transition_items REFERENCING OLD TABLE AS old_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe()",
    );

    exec(
        &engine,
        "INSERT INTO transition_items(id, value) VALUES (1, 10), (2, 20), (3, 30)",
    );
    exec(
        &engine,
        "UPDATE transition_items SET value = value * 2 WHERE id <= 2",
    );
    exec(
        &engine,
        "UPDATE transition_items SET value = value * 2 WHERE id = 999",
    );
    exec(&engine, "DELETE FROM transition_items WHERE id IN (1, 3)");

    assert_eq!(
        strings(
            &engine,
            "SELECT message FROM transition_log ORDER BY id",
            "message"
        ),
        vec![
            "transition_insert_row:INSERT:0:3:0:63",
            "transition_insert_row:INSERT:0:3:0:63",
            "transition_insert_row:INSERT:0:3:0:63",
            "transition_insert_statement:INSERT:0:3:0:63",
            "transition_update_statement:UPDATE:2:2:32:66",
            "transition_update_statement:UPDATE:0:0:0:0",
            "transition_delete_statement:DELETE:2:0:54:0",
        ]
    );
}

#[test]
fn transition_relations_cover_insert_select_on_conflict_update_from_and_merge() {
    let engine = Engine::new();
    install_transition_fixture(&engine);
    for sql in [
        "CREATE TRIGGER transition_insert_statement AFTER INSERT ON transition_items REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe()",
        "CREATE TRIGGER transition_insert_row AFTER INSERT ON transition_items REFERENCING NEW TABLE AS new_rows FOR EACH ROW EXECUTE FUNCTION transition_probe()",
        "CREATE TRIGGER transition_update_statement AFTER UPDATE ON transition_items REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe()",
        "CREATE TRIGGER transition_delete_statement AFTER DELETE ON transition_items REFERENCING OLD TABLE AS old_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe()",
    ] {
        exec(&engine, sql);
    }
    exec(&engine, "INSERT INTO transition_items VALUES (2, 20)");
    exec(&engine, "DELETE FROM transition_log");

    exec(
        &engine,
        "CREATE TABLE transition_insert_source (id INTEGER PRIMARY KEY, value INTEGER); INSERT INTO transition_insert_source VALUES (4, 40), (5, 50); INSERT INTO transition_items(id, value) SELECT id, value FROM transition_insert_source ORDER BY id",
    );
    assert_eq!(
        strings(
            &engine,
            "SELECT message FROM transition_log ORDER BY id",
            "message"
        ),
        vec![
            "transition_insert_row:INSERT:0:2:0:92",
            "transition_insert_row:INSERT:0:2:0:92",
            "transition_insert_statement:INSERT:0:2:0:92",
        ]
    );
    exec(&engine, "DELETE FROM transition_log");

    exec(
        &engine,
        "INSERT INTO transition_items(id, value) VALUES (2, 200), (6, 60) ON CONFLICT (id) DO UPDATE SET value = excluded.value",
    );
    assert_eq!(
        strings(
            &engine,
            "SELECT message FROM transition_log ORDER BY id",
            "message"
        ),
        vec![
            "transition_insert_row:INSERT:0:1:0:61",
            "transition_update_statement:UPDATE:1:1:21:202",
            "transition_insert_statement:INSERT:0:1:0:61",
        ]
    );
    exec(&engine, "DELETE FROM transition_log");

    exec(
        &engine,
        "CREATE TABLE transition_adjustments (id INTEGER PRIMARY KEY, delta INTEGER); INSERT INTO transition_adjustments VALUES (4, 3), (5, 4); UPDATE transition_items AS target SET value = target.value + adjustment.delta FROM transition_adjustments AS adjustment WHERE target.id = adjustment.id",
    );
    assert_eq!(
        strings(
            &engine,
            "SELECT message FROM transition_log ORDER BY id",
            "message"
        ),
        vec!["transition_update_statement:UPDATE:2:2:92:101"]
    );
    exec(&engine, "DELETE FROM transition_log");

    exec(
        &engine,
        "CREATE TABLE transition_merge_source (id INTEGER PRIMARY KEY, value INTEGER); INSERT INTO transition_merge_source VALUES (2, 300), (4, 400), (7, 70); MERGE INTO transition_items AS target USING transition_merge_source AS source ON target.id = source.id WHEN MATCHED AND source.id = 2 THEN DELETE WHEN MATCHED THEN UPDATE SET value = source.value WHEN NOT MATCHED THEN INSERT (id, value) VALUES (source.id, source.value)",
    );
    assert_eq!(
        strings(
            &engine,
            "SELECT message FROM transition_log ORDER BY id",
            "message"
        ),
        vec![
            "transition_insert_row:INSERT:0:1:0:71",
            "transition_delete_statement:DELETE:1:0:202:0",
            "transition_update_statement:UPDATE:1:1:45:401",
            "transition_insert_statement:INSERT:0:1:0:71",
        ]
    );
}

#[test]
fn transition_relations_convert_partition_and_inheritance_rows_to_root_type() {
    let engine = Engine::new();
    install_transition_fixture(&engine);
    exec(
        &engine,
        "CREATE TABLE transition_partitioned (id INTEGER, value INTEGER, generated_value INTEGER GENERATED ALWAYS AS (value * 10) STORED) PARTITION BY RANGE (id); CREATE TABLE transition_partitioned_low PARTITION OF transition_partitioned FOR VALUES FROM (0) TO (10); CREATE TABLE transition_partitioned_high PARTITION OF transition_partitioned FOR VALUES FROM (10) TO (20); CREATE TRIGGER transition_partition_mutate BEFORE INSERT OR UPDATE ON transition_partitioned FOR EACH ROW EXECUTE FUNCTION transition_mutate_row(); CREATE TRIGGER transition_partition_update AFTER UPDATE ON transition_partitioned REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe(); INSERT INTO transition_partitioned(id, value) VALUES (1, 10), (11, 20); DELETE FROM transition_log; UPDATE transition_partitioned SET value = value * 2",
    );
    for sql in [
        "CREATE TRIGGER bad_partitioned_row AFTER UPDATE ON transition_partitioned REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH ROW EXECUTE FUNCTION transition_probe()",
        "CREATE TRIGGER bad_partition_row AFTER UPDATE ON transition_partitioned_low REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH ROW EXECUTE FUNCTION transition_probe()",
    ] {
        let error = engine.sql(sql, &[]).expect_err("partition row transition must fail");
        assert_eq!(error.sqlstate(), Some("0A000"), "{sql}: {error}");
    }
    assert_eq!(
        strings(
            &engine,
            "SELECT message FROM transition_log ORDER BY id",
            "message"
        ),
        vec!["transition_partition_update:UPDATE:2:2:32:66"]
    );

    exec(&engine, "DELETE FROM transition_log");
    exec(
        &engine,
        "CREATE TABLE transition_inherited (id INTEGER, value INTEGER, generated_value INTEGER GENERATED ALWAYS AS (value * 10) STORED); CREATE TABLE transition_inherited_child () INHERITS (transition_inherited); CREATE TRIGGER transition_inherited_update AFTER UPDATE ON transition_inherited REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe(); INSERT INTO transition_inherited(id, value) VALUES (1, 10); INSERT INTO transition_inherited_child(id, value) VALUES (2, 20); UPDATE transition_inherited SET value = value + 1",
    );
    let error = engine
        .sql(
            "CREATE TRIGGER bad_inheritance_child_row AFTER UPDATE ON transition_inherited_child REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH ROW EXECUTE FUNCTION transition_probe()",
            &[],
        )
        .expect_err("inheritance child row transition must fail");
    assert_eq!(error.sqlstate(), Some("0A000"), "{error}");
    assert_eq!(
        strings(
            &engine,
            "SELECT message FROM transition_log ORDER BY id",
            "message"
        ),
        vec!["transition_inherited_update:UPDATE:2:2:30:32"]
    );
}

#[test]
fn transition_relations_collect_foreign_key_cascade_rows() {
    let engine = Engine::new();
    install_transition_fixture(&engine);
    exec(
        &engine,
        "CREATE TABLE transition_referenced (id INTEGER PRIMARY KEY); CREATE TABLE transition_referencing (id INTEGER PRIMARY KEY REFERENCES transition_referenced(id) ON UPDATE CASCADE ON DELETE CASCADE, value INTEGER, generated_value INTEGER GENERATED ALWAYS AS (value * 10) STORED); CREATE TRIGGER transition_cascade_update AFTER UPDATE ON transition_referencing REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe(); CREATE TRIGGER transition_cascade_delete AFTER DELETE ON transition_referencing REFERENCING OLD TABLE AS old_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe(); INSERT INTO transition_referenced VALUES (1), (2); INSERT INTO transition_referencing(id, value) VALUES (1, 10), (2, 20); DELETE FROM transition_log; UPDATE transition_referenced SET id = id + 10",
    );
    assert_eq!(
        strings(
            &engine,
            "SELECT message FROM transition_log ORDER BY id",
            "message"
        ),
        vec!["transition_cascade_update:UPDATE:2:2:30:30"]
    );

    exec(
        &engine,
        "DELETE FROM transition_log; DELETE FROM transition_referenced",
    );
    assert_eq!(
        strings(
            &engine,
            "SELECT message FROM transition_log ORDER BY id",
            "message"
        ),
        vec!["transition_cascade_delete:DELETE:2:0:30:0"]
    );
}

#[test]
fn transition_relations_coalesce_multiple_foreign_key_statement_boundaries() {
    let engine = Engine::new();
    install_transition_fixture(&engine);
    exec(
        &engine,
        "CREATE TABLE transition_multi_parent (a INTEGER UNIQUE, b INTEGER UNIQUE); CREATE TABLE transition_multi_child (id INTEGER PRIMARY KEY, a INTEGER REFERENCES transition_multi_parent(a) ON UPDATE CASCADE, b INTEGER REFERENCES transition_multi_parent(b) ON UPDATE CASCADE, value INTEGER, generated_value INTEGER GENERATED ALWAYS AS (value * 10) STORED); CREATE TABLE transition_multi_statement_log (id BIGSERIAL PRIMARY KEY, message TEXT); CREATE FUNCTION transition_multi_statement_probe() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO transition_multi_statement_log(message) VALUES (TG_NAME || ':' || TG_WHEN || ':' || TG_OP); RETURN NULL; END $$; CREATE TRIGGER transition_multi_cascade_update AFTER UPDATE ON transition_multi_child REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe(); CREATE TRIGGER transition_multi_aa_before_b BEFORE UPDATE OF b ON transition_multi_child FOR EACH STATEMENT EXECUTE FUNCTION transition_multi_statement_probe(); CREATE TRIGGER transition_multi_before BEFORE UPDATE ON transition_multi_child FOR EACH STATEMENT EXECUTE FUNCTION transition_multi_statement_probe(); CREATE TRIGGER transition_multi_before_a BEFORE UPDATE OF a ON transition_multi_child FOR EACH STATEMENT EXECUTE FUNCTION transition_multi_statement_probe(); CREATE TRIGGER transition_multi_aa_after_b AFTER UPDATE OF b ON transition_multi_child FOR EACH STATEMENT EXECUTE FUNCTION transition_multi_statement_probe(); CREATE TRIGGER transition_multi_after AFTER UPDATE ON transition_multi_child FOR EACH STATEMENT EXECUTE FUNCTION transition_multi_statement_probe(); CREATE TRIGGER transition_multi_after_b AFTER UPDATE OF b ON transition_multi_child FOR EACH STATEMENT EXECUTE FUNCTION transition_multi_statement_probe(); INSERT INTO transition_multi_parent VALUES (1, 10), (2, 20); INSERT INTO transition_multi_child(id, a, b, value) VALUES (1, 1, 10, 100), (2, 2, 20, 200); DELETE FROM transition_log; UPDATE transition_multi_parent SET a = a + 100, b = b + 1000",
    );
    assert_eq!(
        strings(
            &engine,
            "SELECT message FROM transition_log ORDER BY id",
            "message"
        ),
        vec!["transition_multi_cascade_update:UPDATE:4:4:600:600"]
    );
    assert_eq!(
        strings(
            &engine,
            "SELECT message FROM transition_multi_statement_log ORDER BY id",
            "message"
        ),
        vec![
            "transition_multi_before:BEFORE:UPDATE",
            "transition_multi_before_a:BEFORE:UPDATE",
            "transition_multi_aa_after_b:AFTER:UPDATE",
            "transition_multi_after:AFTER:UPDATE",
            "transition_multi_after_b:AFTER:UPDATE",
        ]
    );
    assert_eq!(
        strings(
            &engine,
            "SELECT a::text || ':' || b::text AS keys FROM transition_multi_child ORDER BY id",
            "keys"
        ),
        vec!["101:1010", "102:1020"]
    );
}

#[test]
fn transition_relations_cannot_escape_into_persistent_relations() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE transition_persistence_items (value INTEGER)",
    );
    exec(
        &engine,
        "CREATE FUNCTION transition_create_materialized_view() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN CREATE MATERIALIZED VIEW transition_leaked_view AS SELECT * FROM inserted_rows; RETURN NEW; END $$",
    );
    exec(
        &engine,
        "CREATE TRIGGER transition_persistence_guard AFTER INSERT ON transition_persistence_items REFERENCING NEW TABLE AS inserted_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_create_materialized_view()",
    );
    let error = engine
        .sql("INSERT INTO transition_persistence_items VALUES (42)", &[])
        .expect_err("transition relation must not be persisted by a materialized view");
    assert_eq!(error.sqlstate(), Some("0A000"), "{error}");
    let row = exec(
        &engine,
        "SELECT (SELECT count(*) FROM transition_persistence_items) AS item_count, (SELECT count(*) FROM pg_class WHERE relname = 'transition_leaked_view') AS view_count",
    )
    .rows
    .into_iter()
    .next()
    .unwrap();
    assert_eq!(row.get("item_count"), Some(&Value::Int(0)));
    assert_eq!(row.get("view_count"), Some(&Value::Int(0)));
}

#[test]
fn transition_relation_zero_row_update_survives_persistent_reopen() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("transition-reopen.db");
    {
        let engine = Engine::open(&path).unwrap();
        install_transition_fixture(&engine);
        exec(
            &engine,
            "CREATE TRIGGER transition_insert AFTER INSERT ON transition_items REFERENCING NEW TABLE AS new_rows FOR EACH ROW EXECUTE FUNCTION transition_probe(); CREATE TRIGGER transition_update AFTER UPDATE ON transition_items REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe()",
        );
        exec(
            &engine,
            "INSERT INTO transition_items VALUES (1, 10), (2, 20)",
        );
        exec(&engine, "UPDATE transition_items SET value = value * 2");
    }
    let engine = Engine::open(&path).unwrap();
    exec(
        &engine,
        "UPDATE transition_items SET value = value * 2 WHERE id = 999",
    );
    assert_eq!(
        strings(
            &engine,
            "SELECT message FROM transition_log ORDER BY id DESC LIMIT 1",
            "message"
        ),
        vec!["transition_update:UPDATE:0:0:0:0"]
    );
}
