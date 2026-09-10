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
         DECLARE old_count BIGINT := 0; new_count BIGINT := 0; old_sum BIGINT := 0; new_sum BIGINT := 0; old_generated_sum BIGINT := 0; new_generated_sum BIGINT := 0;
         BEGIN
           IF TG_OP IN ('UPDATE', 'DELETE') THEN
             SELECT count(*), coalesce(sum(value), 0), coalesce(sum(generated_value), 0) INTO old_count, old_sum, old_generated_sum FROM old_rows;
           END IF;
           IF TG_OP IN ('INSERT', 'UPDATE') THEN
             SELECT count(*), coalesce(sum(value), 0), coalesce(sum(generated_value), 0) INTO new_count, new_sum, new_generated_sum FROM new_rows;
           END IF;
           INSERT INTO transition_log(message) VALUES (TG_NAME || ':' || TG_OP || ':' || old_count::text || ':' || new_count::text || ':' || old_sum::text || ':' || new_sum::text || ':' || old_generated_sum::text || ':' || new_generated_sum::text);
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
            "CREATE TRIGGER transition_update AFTER UPDATE ON transition_items REFERENCING NEW TABLE AS new_rows OLD TABLE AS old_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe()",
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
fn nested_routines_and_triggers_do_not_inherit_transition_relation_aliases() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE transition_scope_rows (value INTEGER); INSERT INTO transition_scope_rows VALUES (1), (2), (3); CREATE TABLE transition_scope_outer (id INTEGER); CREATE TABLE transition_scope_inner (id INTEGER); CREATE TABLE transition_scope_log (id BIGSERIAL PRIMARY KEY, message TEXT); CREATE FUNCTION transition_scope_helper() RETURNS BIGINT LANGUAGE plpgsql AS $$ DECLARE row_count BIGINT; BEGIN SELECT count(*) INTO row_count FROM transition_scope_rows; RETURN row_count; END $$; CREATE FUNCTION transition_scope_inner_probe() RETURNS trigger LANGUAGE plpgsql AS $$ DECLARE row_count BIGINT; BEGIN SELECT count(*) INTO row_count FROM transition_scope_rows; INSERT INTO transition_scope_log(message) VALUES ('inner:' || row_count::text); RETURN NEW; END $$; CREATE TRIGGER transition_scope_inner_trigger AFTER INSERT ON transition_scope_inner FOR EACH ROW EXECUTE FUNCTION transition_scope_inner_probe(); CREATE FUNCTION transition_scope_outer_probe() RETURNS trigger LANGUAGE plpgsql AS $$ DECLARE before_count BIGINT; helper_count BIGINT; after_count BIGINT; BEGIN SELECT count(*) INTO before_count FROM transition_scope_rows; helper_count := transition_scope_helper(); INSERT INTO transition_scope_inner VALUES (1); SELECT count(*) INTO after_count FROM transition_scope_rows; INSERT INTO transition_scope_log(message) VALUES ('outer:' || before_count::text || ':' || helper_count::text || ':' || after_count::text); RETURN NULL; END $$; CREATE TRIGGER transition_scope_outer_trigger AFTER INSERT ON transition_scope_outer REFERENCING NEW TABLE AS transition_scope_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_scope_outer_probe(); INSERT INTO transition_scope_outer VALUES (1), (2)",
    );
    assert_eq!(
        strings(
            &engine,
            "SELECT message FROM transition_scope_log ORDER BY id",
            "message"
        ),
        vec!["inner:3", "outer:2:3:2"]
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
            "transition_insert_row:INSERT:0:3:0:63:0:630",
            "transition_insert_row:INSERT:0:3:0:63:0:630",
            "transition_insert_row:INSERT:0:3:0:63:0:630",
            "transition_insert_statement:INSERT:0:3:0:63:0:630",
            "transition_update_statement:UPDATE:2:2:32:66:320:660",
            "transition_update_statement:UPDATE:0:0:0:0:0:0",
            "transition_delete_statement:DELETE:2:0:54:0:540:0",
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
            "transition_insert_row:INSERT:0:2:0:92:0:920",
            "transition_insert_row:INSERT:0:2:0:92:0:920",
            "transition_insert_statement:INSERT:0:2:0:92:0:920",
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
            "transition_insert_row:INSERT:0:1:0:61:0:610",
            "transition_update_statement:UPDATE:1:1:21:202:210:2020",
            "transition_insert_statement:INSERT:0:1:0:61:0:610",
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
        vec!["transition_update_statement:UPDATE:2:2:92:101:920:1010"]
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
            "transition_insert_row:INSERT:0:1:0:71:0:710",
            "transition_delete_statement:DELETE:1:0:202:0:2020:0",
            "transition_update_statement:UPDATE:1:1:45:401:450:4010",
            "transition_insert_statement:INSERT:0:1:0:71:0:710",
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
        vec!["transition_partition_update:UPDATE:2:2:32:66:320:660"]
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
        vec!["transition_inherited_update:UPDATE:2:2:30:32:300:320"]
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
        vec!["transition_cascade_update:UPDATE:2:2:30:30:300:300"]
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
        vec!["transition_cascade_delete:DELETE:2:0:30:0:300:0"]
    );
}

#[test]
fn transition_relations_coalesce_multiple_foreign_key_statement_boundaries() {
    let engine = Engine::new();
    install_transition_fixture(&engine);
    exec(
        &engine,
        "CREATE TABLE transition_multi_parent (a INTEGER UNIQUE, b INTEGER UNIQUE); CREATE TABLE transition_multi_child (id INTEGER PRIMARY KEY, a INTEGER REFERENCES transition_multi_parent(a) ON UPDATE CASCADE, b INTEGER REFERENCES transition_multi_parent(b) ON UPDATE CASCADE, value INTEGER, generated_value INTEGER GENERATED ALWAYS AS (value * 10) STORED); CREATE TABLE transition_multi_statement_log (id BIGSERIAL PRIMARY KEY, message TEXT); CREATE FUNCTION transition_multi_statement_probe() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO transition_multi_statement_log(message) VALUES (TG_NAME || ':' || TG_WHEN || ':' || TG_OP); RETURN NULL; END $$; CREATE TRIGGER transition_multi_cascade_row AFTER UPDATE ON transition_multi_child REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH ROW EXECUTE FUNCTION transition_probe(); CREATE TRIGGER transition_multi_cascade_update AFTER UPDATE ON transition_multi_child REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe(); CREATE TRIGGER transition_multi_aa_before_b BEFORE UPDATE OF b ON transition_multi_child FOR EACH STATEMENT EXECUTE FUNCTION transition_multi_statement_probe(); CREATE TRIGGER transition_multi_before BEFORE UPDATE ON transition_multi_child FOR EACH STATEMENT EXECUTE FUNCTION transition_multi_statement_probe(); CREATE TRIGGER transition_multi_before_a BEFORE UPDATE OF a ON transition_multi_child FOR EACH STATEMENT EXECUTE FUNCTION transition_multi_statement_probe(); CREATE TRIGGER transition_multi_aa_after_b AFTER UPDATE OF b ON transition_multi_child FOR EACH STATEMENT EXECUTE FUNCTION transition_multi_statement_probe(); CREATE TRIGGER transition_multi_after AFTER UPDATE ON transition_multi_child FOR EACH STATEMENT EXECUTE FUNCTION transition_multi_statement_probe(); CREATE TRIGGER transition_multi_after_b AFTER UPDATE OF b ON transition_multi_child FOR EACH STATEMENT EXECUTE FUNCTION transition_multi_statement_probe(); INSERT INTO transition_multi_parent VALUES (1, 10), (2, 20); INSERT INTO transition_multi_child(id, a, b, value) VALUES (1, 1, 10, 100), (2, 2, 20, 200); DELETE FROM transition_log; UPDATE transition_multi_parent SET a = a + 100, b = b + 1000",
    );
    assert_eq!(
        strings(
            &engine,
            "SELECT message FROM transition_log ORDER BY id",
            "message"
        ),
        vec![
            "transition_multi_cascade_row:UPDATE:4:4:600:600:6000:6000",
            "transition_multi_cascade_row:UPDATE:4:4:600:600:6000:6000",
            "transition_multi_cascade_row:UPDATE:4:4:600:600:6000:6000",
            "transition_multi_cascade_row:UPDATE:4:4:600:600:6000:6000",
            "transition_multi_cascade_update:UPDATE:4:4:600:600:6000:6000",
        ]
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
fn self_referential_cascade_splits_transition_sets_for_after_row_triggers() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE transition_self_ref (a INTEGER PRIMARY KEY, b INTEGER REFERENCES transition_self_ref(a) ON DELETE CASCADE); CREATE TABLE transition_self_ref_log (id BIGSERIAL PRIMARY KEY, trigger_name TEXT, old_count BIGINT, old_a_sum BIGINT, old_b_sum BIGINT); CREATE FUNCTION transition_self_ref_probe() RETURNS trigger LANGUAGE plpgsql AS $$ DECLARE row_count BIGINT; a_sum BIGINT; b_sum BIGINT; BEGIN SELECT count(*), coalesce(sum(a), 0), coalesce(sum(b), 0) INTO row_count, a_sum, b_sum FROM old_rows; INSERT INTO transition_self_ref_log(trigger_name, old_count, old_a_sum, old_b_sum) VALUES (TG_NAME, row_count, a_sum, b_sum); RETURN NULL; END $$; CREATE TRIGGER transition_self_ref_row AFTER DELETE ON transition_self_ref REFERENCING OLD TABLE AS old_rows FOR EACH ROW EXECUTE FUNCTION transition_self_ref_probe(); CREATE TRIGGER transition_self_ref_statement AFTER DELETE ON transition_self_ref REFERENCING OLD TABLE AS old_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_self_ref_probe(); INSERT INTO transition_self_ref VALUES (1, NULL), (2, 1), (3, 2); DELETE FROM transition_self_ref WHERE a = 1",
    );
    assert_eq!(
        strings(
            &engine,
            "SELECT trigger_name || ':' || old_count::text || ':' || old_a_sum::text || ':' || old_b_sum::text AS message FROM transition_self_ref_log ORDER BY id",
            "message"
        ),
        vec![
            "transition_self_ref_row:2:3:1",
            "transition_self_ref_row:2:3:1",
            "transition_self_ref_statement:2:3:1",
            "transition_self_ref_row:1:3:2",
            "transition_self_ref_statement:1:3:2",
        ]
    );

    exec(
        &engine,
        "DELETE FROM transition_self_ref_log; DROP TRIGGER transition_self_ref_row ON transition_self_ref; INSERT INTO transition_self_ref VALUES (1, NULL), (2, 1), (3, 2), (4, 3); DELETE FROM transition_self_ref WHERE a = 1",
    );
    assert_eq!(
        strings(
            &engine,
            "SELECT trigger_name || ':' || old_count::text || ':' || old_a_sum::text || ':' || old_b_sum::text AS message FROM transition_self_ref_log ORDER BY id",
            "message"
        ),
        vec!["transition_self_ref_statement:4:10:6"]
    );

    exec(
        &engine,
        "DELETE FROM transition_self_ref_log; CREATE TABLE transition_self_ref_branch (a INTEGER PRIMARY KEY, b INTEGER REFERENCES transition_self_ref_branch(a) ON DELETE CASCADE); CREATE TRIGGER transition_self_ref_branch_row AFTER DELETE ON transition_self_ref_branch REFERENCING OLD TABLE AS old_rows FOR EACH ROW EXECUTE FUNCTION transition_self_ref_probe(); CREATE TRIGGER transition_self_ref_branch_statement AFTER DELETE ON transition_self_ref_branch REFERENCING OLD TABLE AS old_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_self_ref_probe(); INSERT INTO transition_self_ref_branch VALUES (1, NULL), (2, 1), (3, 1), (4, 2), (5, 3); DELETE FROM transition_self_ref_branch WHERE a = 1",
    );
    assert_eq!(
        strings(
            &engine,
            "SELECT trigger_name || ':' || old_count::text || ':' || old_a_sum::text || ':' || old_b_sum::text AS message FROM transition_self_ref_log ORDER BY id",
            "message"
        ),
        vec![
            "transition_self_ref_branch_row:3:6:2",
            "transition_self_ref_branch_row:3:6:2",
            "transition_self_ref_branch_row:3:6:2",
            "transition_self_ref_branch_statement:3:6:2",
            "transition_self_ref_branch_row:2:9:5",
            "transition_self_ref_branch_row:2:9:5",
            "transition_self_ref_branch_statement:2:9:5",
            "transition_self_ref_branch_statement:0:0:0",
        ]
    );

    exec(
        &engine,
        "DELETE FROM transition_self_ref_log; CREATE TABLE transition_self_ref_deep (a INTEGER PRIMARY KEY, b INTEGER REFERENCES transition_self_ref_deep(a) ON DELETE CASCADE); CREATE TRIGGER transition_self_ref_deep_row AFTER DELETE ON transition_self_ref_deep REFERENCING OLD TABLE AS old_rows FOR EACH ROW EXECUTE FUNCTION transition_self_ref_probe(); CREATE TRIGGER transition_self_ref_deep_statement AFTER DELETE ON transition_self_ref_deep REFERENCING OLD TABLE AS old_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_self_ref_probe(); INSERT INTO transition_self_ref_deep VALUES (1, NULL), (2, 1), (3, 2), (4, 3); DELETE FROM transition_self_ref_deep WHERE a = 1",
    );
    assert_eq!(
        strings(
            &engine,
            "SELECT trigger_name || ':' || old_count::text || ':' || old_a_sum::text || ':' || old_b_sum::text AS message FROM transition_self_ref_log ORDER BY id",
            "message"
        ),
        vec![
            "transition_self_ref_deep_row:2:3:1",
            "transition_self_ref_deep_row:2:3:1",
            "transition_self_ref_deep_statement:2:3:1",
            "transition_self_ref_deep_row:2:7:5",
            "transition_self_ref_deep_row:2:7:5",
            "transition_self_ref_deep_statement:2:7:5",
            "transition_self_ref_deep_statement:0:0:0",
        ]
    );
}

#[test]
fn multi_row_conflict_cascades_rebase_transition_event_sequences() {
    let engine = Engine::new();
    install_transition_fixture(&engine);
    exec(
        &engine,
        "CREATE TABLE transition_conflict_chain (a INTEGER PRIMARY KEY, b INTEGER UNIQUE, c INTEGER, replacement INTEGER, value INTEGER, generated_value INTEGER GENERATED ALWAYS AS (value * 10) STORED, FOREIGN KEY (b) REFERENCES transition_conflict_chain(a) ON UPDATE CASCADE, FOREIGN KEY (c) REFERENCES transition_conflict_chain(b) ON UPDATE CASCADE); CREATE TRIGGER transition_conflict_chain_row AFTER UPDATE ON transition_conflict_chain REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH ROW EXECUTE FUNCTION transition_probe(); CREATE TRIGGER transition_conflict_chain_statement AFTER UPDATE ON transition_conflict_chain REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe(); INSERT INTO transition_conflict_chain(a, b, c, value) VALUES (1, NULL, NULL, 10), (2, 1, NULL, 20), (3, NULL, 1, 30), (10, NULL, NULL, 100); INSERT INTO transition_conflict_chain(a, b, c, replacement, value) VALUES (10, NULL, NULL, 110, 100), (1, NULL, NULL, 101, 10) ON CONFLICT (a) DO UPDATE SET a = excluded.replacement",
    );
    assert_eq!(
        strings(
            &engine,
            "SELECT message FROM transition_log ORDER BY id",
            "message"
        ),
        vec![
            "transition_conflict_chain_row:UPDATE:2:2:110:110:1100:1100",
            "transition_conflict_chain_row:UPDATE:2:2:110:110:1100:1100",
            "transition_conflict_chain_statement:UPDATE:2:2:110:110:1100:1100",
            "transition_conflict_chain_row:UPDATE:2:2:50:50:500:500",
            "transition_conflict_chain_row:UPDATE:2:2:50:50:500:500",
            "transition_conflict_chain_statement:UPDATE:2:2:50:50:500:500",
        ]
    );
    assert_eq!(
        strings(
            &engine,
            "SELECT a::text || ':' || coalesce(b::text, 'NULL') || ':' || coalesce(c::text, 'NULL') || ':' || value::text || ':' || generated_value::text AS row_image FROM transition_conflict_chain ORDER BY a",
            "row_image"
        ),
        vec![
            "2:101:NULL:20:200",
            "3:NULL:101:30:300",
            "101:NULL:NULL:10:100",
            "110:NULL:NULL:100:1000",
        ]
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
        vec!["transition_update:UPDATE:0:0:0:0:0:0"]
    );
}
