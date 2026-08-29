//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 constraint-trigger deferral, transaction, and catalog coverage.

use super::*;

fn install_constraint_trigger_fixture(engine: &Engine) {
    exec(
        engine,
        "CREATE TABLE guarded_items (id INTEGER PRIMARY KEY, value INTEGER)",
    );
    exec(
        engine,
        "CREATE TABLE constraint_trigger_log (
           sequence BIGSERIAL PRIMARY KEY,
           message TEXT
         )",
    );
    exec(
        engine,
        "CREATE FUNCTION log_constraint_trigger() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           INSERT INTO constraint_trigger_log(message) VALUES
             (TG_NAME || ':' || TG_OP || ':' || coalesce(OLD.value::text, 'NULL') || ':' || coalesce(NEW.value::text, 'NULL'));
           RETURN NULL;
         END
         $$",
    );
    exec(
        engine,
        "CREATE CONSTRAINT TRIGGER guarded_items_check
         AFTER INSERT OR UPDATE OR DELETE ON guarded_items
         DEFERRABLE INITIALLY DEFERRED
         FOR EACH ROW EXECUTE FUNCTION log_constraint_trigger()",
    );
}

#[test]
fn deferred_constraint_triggers_fire_retroactively_and_follow_the_transaction_mode() {
    let engine = Engine::new();
    install_constraint_trigger_fixture(&engine);

    exec(&engine, "BEGIN");
    exec(&engine, "INSERT INTO guarded_items VALUES (1, 10)");
    assert!(exec(&engine, "SELECT * FROM constraint_trigger_log")
        .rows
        .is_empty());
    exec(&engine, "SET CONSTRAINTS guarded_items_check IMMEDIATE");
    assert_eq!(
        strings(
            &engine,
            "SELECT message FROM constraint_trigger_log ORDER BY sequence",
            "message",
        ),
        ["guarded_items_check:INSERT:NULL:10"]
    );
    exec(&engine, "UPDATE guarded_items SET value = 20 WHERE id = 1");
    exec(&engine, "DELETE FROM guarded_items WHERE id = 1");
    assert_eq!(
        strings(
            &engine,
            "SELECT message FROM constraint_trigger_log ORDER BY sequence",
            "message",
        ),
        [
            "guarded_items_check:INSERT:NULL:10",
            "guarded_items_check:UPDATE:10:20",
            "guarded_items_check:DELETE:20:NULL",
        ]
    );
    exec(&engine, "COMMIT");
}

#[test]
fn deferred_constraint_trigger_events_follow_savepoint_rollback_commit_and_drop() {
    let engine = Engine::new();
    install_constraint_trigger_fixture(&engine);

    exec(&engine, "BEGIN");
    exec(&engine, "INSERT INTO guarded_items VALUES (1, 10)");
    exec(&engine, "SAVEPOINT discard_event");
    exec(&engine, "INSERT INTO guarded_items VALUES (2, 20)");
    exec(&engine, "ROLLBACK TO SAVEPOINT discard_event");
    exec(&engine, "COMMIT");
    assert_eq!(
        strings(
            &engine,
            "SELECT message FROM constraint_trigger_log ORDER BY sequence",
            "message",
        ),
        ["guarded_items_check:INSERT:NULL:10"]
    );

    exec(&engine, "TRUNCATE constraint_trigger_log RESTART IDENTITY");
    exec(&engine, "BEGIN");
    exec(&engine, "INSERT INTO guarded_items VALUES (3, 30)");
    exec(&engine, "DROP TRIGGER guarded_items_check ON guarded_items");
    exec(&engine, "COMMIT");
    assert!(exec(&engine, "SELECT * FROM constraint_trigger_log")
        .rows
        .is_empty());
}

#[test]
fn constraint_trigger_catalogs_partition_clones_and_definition_match_postgresql_18() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE referenced_items (id INTEGER PRIMARY KEY)",
    );
    exec(
        &engine,
        "CREATE TABLE partitioned_guarded_items (id INTEGER, value INTEGER) PARTITION BY RANGE (id)",
    );
    exec(
        &engine,
        "CREATE TABLE partitioned_guarded_items_low PARTITION OF partitioned_guarded_items FOR VALUES FROM (0) TO (10)",
    );
    exec(
        &engine,
        "CREATE FUNCTION catalog_constraint_trigger() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN RETURN NULL; END
         $$",
    );
    exec(
        &engine,
        "CREATE CONSTRAINT TRIGGER partitioned_guard
         AFTER INSERT OR UPDATE OR DELETE ON partitioned_guarded_items
         FROM referenced_items DEFERRABLE INITIALLY DEFERRED
         FOR EACH ROW EXECUTE FUNCTION catalog_constraint_trigger('argument')",
    );

    let rows = exec(
        &engine,
        "SELECT c.relname, t.oid AS trigger_oid, t.tgparentid, t.tgtype,
                t.tgconstrrelid, t.tgconstraint, t.tgdeferrable, t.tginitdeferred,
                pc.oid AS constraint_oid, pc.conname, pc.contype, pc.conrelid,
                pc.confrelid, pc.condeferrable, pc.condeferred, pc.connoinherit
         FROM pg_trigger t
         JOIN pg_class c ON c.oid = t.tgrelid
         JOIN pg_constraint pc ON pc.oid = t.tgconstraint
         WHERE t.tgname = 'partitioned_guard'
         ORDER BY c.relname",
    )
    .rows;
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0].get("relname"),
        Some(&Value::Str("partitioned_guarded_items".into()))
    );
    assert_eq!(rows[0].get("tgparentid"), Some(&Value::Int(0)));
    assert_eq!(rows[0].get("tgtype"), Some(&Value::Int(29)));
    assert!(matches!(rows[0].get("tgconstrrelid"), Some(Value::Int(value)) if *value != 0));
    assert_eq!(rows[0].get("tgconstraint"), rows[0].get("constraint_oid"));
    assert_eq!(
        rows[0].get("conname"),
        Some(&Value::Str("partitioned_guard".into()))
    );
    assert_eq!(rows[0].get("contype"), Some(&Value::Str("t".into())));
    assert_eq!(rows[0].get("confrelid"), Some(&Value::Int(0)));
    assert_eq!(rows[0].get("tgdeferrable"), Some(&Value::Bool(true)));
    assert_eq!(rows[0].get("tginitdeferred"), Some(&Value::Bool(true)));
    assert_eq!(rows[0].get("condeferrable"), Some(&Value::Bool(true)));
    assert_eq!(rows[0].get("condeferred"), Some(&Value::Bool(true)));
    assert_eq!(rows[0].get("connoinherit"), Some(&Value::Bool(true)));
    assert!(matches!(rows[1].get("tgparentid"), Some(Value::Int(value)) if *value != 0));
    assert_ne!(rows[0].get("trigger_oid"), rows[1].get("trigger_oid"));
    assert_ne!(rows[0].get("constraint_oid"), rows[1].get("constraint_oid"));

    let definition = strings(
        &engine,
        "SELECT pg_get_triggerdef(oid, false) AS definition
         FROM pg_trigger
         WHERE tgname = 'partitioned_guard' AND tgparentid = 0",
        "definition",
    );
    assert_eq!(definition.len(), 1);
    assert!(definition[0].contains("CREATE CONSTRAINT TRIGGER partitioned_guard"));
    assert!(definition[0].contains("FROM referenced_items"));
    assert!(definition[0].contains("DEFERRABLE INITIALLY DEFERRED"));
    assert!(definition[0].contains("FOR EACH ROW"));
}

#[test]
fn trigger_and_constraint_names_have_independent_durable_lifecycle() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("constraint-trigger-lifecycle.db");
    let engine = Engine::open(&database).unwrap();
    install_constraint_trigger_fixture(&engine);
    let before = exec(
        &engine,
        "SELECT t.oid AS trigger_oid, c.oid AS constraint_oid
         FROM pg_trigger t JOIN pg_constraint c ON c.oid = t.tgconstraint
         WHERE t.tgname = 'guarded_items_check'",
    )
    .rows
    .remove(0);

    exec(&engine, "BEGIN");
    exec(&engine, "INSERT INTO guarded_items VALUES (1, 10)");
    exec(
        &engine,
        "ALTER TRIGGER guarded_items_check ON guarded_items RENAME TO renamed_trigger",
    );
    let renamed = exec(
        &engine,
        "SELECT t.oid AS trigger_oid, t.tgname, c.oid AS constraint_oid, c.conname
         FROM pg_trigger t JOIN pg_constraint c ON c.oid = t.tgconstraint
         WHERE t.tgname = 'renamed_trigger'",
    )
    .rows
    .remove(0);
    assert_eq!(renamed.get("trigger_oid"), before.get("trigger_oid"));
    assert_eq!(renamed.get("constraint_oid"), before.get("constraint_oid"));
    assert_eq!(
        renamed.get("conname"),
        Some(&Value::Str("guarded_items_check".into()))
    );
    exec(&engine, "SET CONSTRAINTS guarded_items_check IMMEDIATE");
    assert_eq!(
        strings(
            &engine,
            "SELECT message FROM constraint_trigger_log ORDER BY sequence",
            "message",
        ),
        ["renamed_trigger:INSERT:NULL:10"]
    );
    exec(
        &engine,
        "ALTER TABLE guarded_items RENAME CONSTRAINT guarded_items_check TO renamed_constraint",
    );
    let renamed_constraint = exec(
        &engine,
        "SELECT t.oid AS trigger_oid, t.tgname, c.oid AS constraint_oid, c.conname
         FROM pg_trigger t JOIN pg_constraint c ON c.oid = t.tgconstraint
         WHERE t.tgname = 'renamed_trigger'",
    )
    .rows
    .remove(0);
    assert_eq!(
        renamed_constraint.get("trigger_oid"),
        before.get("trigger_oid")
    );
    assert_eq!(
        renamed_constraint.get("constraint_oid"),
        before.get("constraint_oid")
    );
    assert_eq!(
        renamed_constraint.get("conname"),
        Some(&Value::Str("renamed_constraint".into()))
    );
    let drop_error = engine
        .sql(
            "ALTER TABLE guarded_items DROP CONSTRAINT renamed_constraint CASCADE",
            &[],
        )
        .expect_err("a constraint trigger owns its pg_constraint row");
    assert_eq!(drop_error.sqlstate(), Some("2BP01"));
    exec(&engine, "ROLLBACK");

    drop(engine);
    let reopened = Engine::open(&database).unwrap();
    assert_eq!(
        exec(
            &reopened,
            "SELECT count(*) AS n FROM pg_trigger WHERE tgname = 'guarded_items_check'",
        )
        .rows[0]
            .get("n"),
        Some(&Value::Int(1))
    );
}

#[test]
fn deferred_constraint_trigger_failure_rolls_back_the_outer_transaction() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE guarded_failures (id INTEGER PRIMARY KEY, value INTEGER)",
    );
    exec(
        &engine,
        "CREATE FUNCTION reject_negative_constraint_trigger() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           IF NEW.value < 0 THEN RAISE EXCEPTION 'negative value'; END IF;
           RETURN NULL;
         END
         $$",
    );
    exec(
        &engine,
        "CREATE CONSTRAINT TRIGGER reject_negative
         AFTER INSERT OR UPDATE ON guarded_failures
         DEFERRABLE INITIALLY DEFERRED FOR EACH ROW
         EXECUTE FUNCTION reject_negative_constraint_trigger()",
    );
    exec(&engine, "BEGIN");
    exec(&engine, "INSERT INTO guarded_failures VALUES (1, -1)");
    let error = engine
        .sql("COMMIT", &[])
        .expect_err("commit must fire the trigger");
    assert_eq!(error.sqlstate(), Some("P0001"));
    assert!(exec(&engine, "SELECT * FROM guarded_failures")
        .rows
        .is_empty());
}

#[test]
fn dropping_a_constraint_trigger_referenced_relation_removes_the_trigger() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE constraint_trigger_target (id INTEGER)",
    );
    exec(
        &engine,
        "CREATE TABLE constraint_trigger_reference (id INTEGER)",
    );
    exec(
        &engine,
        "CREATE FUNCTION referenced_constraint_trigger() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN RETURN NULL; END
         $$",
    );
    exec(
        &engine,
        "CREATE CONSTRAINT TRIGGER referenced_guard
         AFTER INSERT ON constraint_trigger_target FROM constraint_trigger_reference
         DEFERRABLE FOR EACH ROW EXECUTE FUNCTION referenced_constraint_trigger()",
    );
    exec(&engine, "DROP TABLE constraint_trigger_reference");
    assert_eq!(
        exec(
            &engine,
            "SELECT count(*) AS n FROM pg_trigger WHERE tgname = 'referenced_guard'",
        )
        .rows[0]
            .get("n"),
        Some(&Value::Int(0))
    );
}

#[test]
fn dropping_a_referenced_relation_cancels_pending_constraint_trigger_events() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE constraint_trigger_target (id INTEGER)",
    );
    exec(
        &engine,
        "CREATE TABLE constraint_trigger_reference (id INTEGER)",
    );
    exec(&engine, "CREATE TABLE constraint_trigger_log (id INTEGER)");
    exec(
        &engine,
        "CREATE FUNCTION pending_referenced_constraint_trigger() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           INSERT INTO constraint_trigger_log VALUES (NEW.id);
           RETURN NULL;
         END
         $$",
    );
    exec(
        &engine,
        "CREATE CONSTRAINT TRIGGER referenced_guard
         AFTER INSERT ON constraint_trigger_target FROM constraint_trigger_reference
         DEFERRABLE INITIALLY DEFERRED FOR EACH ROW
         EXECUTE FUNCTION pending_referenced_constraint_trigger()",
    );

    exec(&engine, "BEGIN");
    exec(&engine, "INSERT INTO constraint_trigger_target VALUES (7)");
    exec(&engine, "DROP TABLE constraint_trigger_reference");
    exec(&engine, "COMMIT");

    assert!(exec(&engine, "SELECT * FROM constraint_trigger_log")
        .rows
        .is_empty());
    assert_eq!(
        exec(
            &engine,
            "SELECT count(*) AS n FROM pg_trigger WHERE tgname = 'referenced_guard'",
        )
        .rows[0]
            .get("n"),
        Some(&Value::Int(0))
    );
}

#[test]
fn legacy_trigger_catalogs_gain_a_stable_object_identity() {
    use uqa_storage::{Catalog, ManagedConnection};

    let directory = TempDir::new().unwrap();
    let database = directory.path().join("legacy-trigger-object-id.db");
    {
        let engine = Engine::open(&database).unwrap();
        exec(&engine, "CREATE TABLE legacy_trigger_items (id INTEGER)");
        exec(
            &engine,
            "CREATE FUNCTION legacy_trigger_probe() RETURNS trigger LANGUAGE plpgsql AS $$
             BEGIN RETURN NEW; END
             $$",
        );
        exec(
            &engine,
            "CREATE TRIGGER legacy_trigger BEFORE INSERT ON legacy_trigger_items
             FOR EACH ROW EXECUTE FUNCTION legacy_trigger_probe()",
        );
    }

    {
        let catalog = Catalog::open(ManagedConnection::open(&database).unwrap()).unwrap();
        let encoded = catalog.get_metadata("sql_triggers_json").unwrap().unwrap();
        let mut metadata: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        let triggers = metadata
            .get_mut("triggers")
            .and_then(serde_json::Value::as_array_mut)
            .unwrap();
        assert_eq!(triggers.len(), 1);
        assert!(triggers[0]
            .as_object_mut()
            .unwrap()
            .remove("object_id")
            .is_some());
        catalog
            .set_metadata(
                "sql_triggers_json",
                &serde_json::to_string(&metadata).unwrap(),
            )
            .unwrap();
    }

    let engine = Engine::open(&database).unwrap();
    let migrated_oid = exec(
        &engine,
        "SELECT oid FROM pg_trigger WHERE tgname = 'legacy_trigger'",
    )
    .rows[0]
        .get("oid")
        .cloned();
    exec(
        &engine,
        "ALTER TRIGGER legacy_trigger ON legacy_trigger_items RENAME TO migrated_trigger",
    );
    assert_eq!(
        exec(
            &engine,
            "SELECT oid FROM pg_trigger WHERE tgname = 'migrated_trigger'",
        )
        .rows[0]
            .get("oid"),
        migrated_oid.as_ref()
    );
    drop(engine);

    let reopened = Engine::open(&database).unwrap();
    assert_eq!(
        exec(
            &reopened,
            "SELECT oid FROM pg_trigger WHERE tgname = 'migrated_trigger'",
        )
        .rows[0]
            .get("oid"),
        migrated_oid.as_ref()
    );
}
