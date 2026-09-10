//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn persistent_rule_catalog_restoration_does_not_reenter_transaction_snapshots() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("rule-catalog-restoration.db");
    let engine = Engine::open(&database).unwrap();
    exec(&engine, "CREATE TABLE view_action_base (id INTEGER)");
    exec(
        &engine,
        "CREATE VIEW view_action_target AS SELECT id FROM view_action_base",
    );
    exec(&engine, "CREATE TABLE view_action_event (id INTEGER)");
    exec(
        &engine,
        "CREATE RULE view_action_rule AS ON INSERT TO view_action_event
         DO ALSO INSERT INTO view_action_target VALUES (NEW.id)",
    );
    exec(
        &engine,
        "CREATE TABLE returning_validation_event (id INTEGER, note VARCHAR(3))",
    );
    exec(
        &engine,
        "CREATE TABLE returning_validation_action (id BIGINT, note VARCHAR(20))",
    );

    let error = engine
        .sql(
            "CREATE RULE returning_wrong_type AS ON INSERT TO returning_validation_event
             DO INSTEAD INSERT INTO returning_validation_action VALUES (NEW.id, NEW.note)
             RETURNING id, note::VARCHAR(3)",
            &[],
        )
        .expect_err("an incompatible rule RETURNING type must be rejected");
    assert_eq!(error.sqlstate(), Some("42P17"));
    assert_eq!(
        exec(
            &engine,
            "SELECT count(*) AS n FROM pg_rewrite WHERE rulename = 'view_action_rule'",
        )
        .rows[0]
            .get("n"),
        Some(&Value::Int(1))
    );

    exec(&engine, "BEGIN");
    exec(&engine, "CREATE TABLE rolled_back_rule_marker (id INTEGER)");
    exec(&engine, "ROLLBACK");
    let missing = engine
        .sql("SELECT * FROM rolled_back_rule_marker", &[])
        .expect_err("explicit rollback must restore the persistent catalog");
    assert_eq!(missing.sqlstate(), Some("42P01"));
}

#[test]
fn insert_rule_returning_maps_provider_rows_to_the_event_relation() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE insert_returning_event (z INTEGER, a TEXT)",
    );
    exec(
        &engine,
        "CREATE TABLE insert_returning_action (mapped_z INTEGER, mapped_a TEXT)",
    );
    exec(
        &engine,
        "CREATE RULE insert_returning_provider AS ON INSERT TO insert_returning_event DO INSTEAD INSERT INTO insert_returning_action VALUES (NEW.z, NEW.a) RETURNING mapped_z + 10, mapped_a || '!'",
    );

    let result = exec(
        &engine,
        "INSERT INTO insert_returning_event VALUES (1, 'one'), (2, 'two') RETURNING old.z AS old_z, new.z AS new_z, z * 2 AS doubled, a",
    );
    assert_eq!(result.affected_rows, 2);
    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.value_at(0, 0), Some(&Value::Null));
    assert_eq!(result.value_at(0, 1), Some(&Value::Int(11)));
    assert_eq!(result.value_at(0, 2), Some(&Value::Int(22)));
    assert_eq!(result.value_at(0, 3), Some(&Value::Str("one!".into())));
    assert_eq!(result.value_at(1, 1), Some(&Value::Int(12)));
    assert_eq!(result.value_at(1, 2), Some(&Value::Int(24)));
    assert_eq!(result.value_at(1, 3), Some(&Value::Str("two!".into())));
    assert!(exec(&engine, "SELECT * FROM insert_returning_event")
        .rows
        .is_empty());
    assert_eq!(
        exec(
            &engine,
            "SELECT mapped_z FROM insert_returning_action ORDER BY mapped_z"
        )
        .rows
        .iter()
        .map(|row| row.get("mapped_z"))
        .collect::<Vec<_>>(),
        [Some(&Value::Int(1)), Some(&Value::Int(2))]
    );
}

#[test]
fn update_and_delete_rule_returning_preserve_action_old_and_new_images() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE update_returning_event (id INTEGER PRIMARY KEY, value INTEGER)",
    );
    exec(
        &engine,
        "CREATE TABLE update_returning_action (id INTEGER PRIMARY KEY, mapped INTEGER)",
    );
    exec(&engine, "INSERT INTO update_returning_event VALUES (1, 10)");
    exec(
        &engine,
        "INSERT INTO update_returning_action VALUES (1, 100)",
    );
    exec(
        &engine,
        "CREATE RULE update_returning_provider AS ON UPDATE TO update_returning_event DO INSTEAD UPDATE update_returning_action SET mapped = NEW.value + 10 WHERE id = OLD.id RETURNING id, mapped + 100",
    );

    let updated = exec(
        &engine,
        "UPDATE update_returning_event SET value = value + 1 RETURNING old.value AS old_value, new.value AS new_value, value",
    );
    assert_eq!(updated.affected_rows, 1);
    assert_eq!(updated.value_at(0, 0), Some(&Value::Int(200)));
    assert_eq!(updated.value_at(0, 1), Some(&Value::Int(121)));
    assert_eq!(updated.value_at(0, 2), Some(&Value::Int(121)));
    assert_eq!(
        exec(&engine, "SELECT value FROM update_returning_event").value_at(0, 0),
        Some(&Value::Int(10))
    );
    assert_eq!(
        exec(&engine, "SELECT mapped FROM update_returning_action").value_at(0, 0),
        Some(&Value::Int(21))
    );

    exec(
        &engine,
        "CREATE TABLE delete_returning_event (id INTEGER PRIMARY KEY, value INTEGER)",
    );
    exec(
        &engine,
        "CREATE TABLE delete_returning_action (id INTEGER PRIMARY KEY, mapped INTEGER)",
    );
    exec(&engine, "INSERT INTO delete_returning_event VALUES (1, 10)");
    exec(
        &engine,
        "INSERT INTO delete_returning_action VALUES (1, 100)",
    );
    exec(
        &engine,
        "CREATE RULE delete_returning_provider AS ON DELETE TO delete_returning_event DO INSTEAD DELETE FROM delete_returning_action WHERE id = OLD.id RETURNING id, mapped + 10",
    );
    let deleted = exec(
        &engine,
        "DELETE FROM delete_returning_event RETURNING old.value AS old_value, new.value AS new_value, value",
    );
    assert_eq!(deleted.affected_rows, 1);
    assert_eq!(deleted.value_at(0, 0), Some(&Value::Int(110)));
    assert_eq!(deleted.value_at(0, 1), Some(&Value::Null));
    assert_eq!(deleted.value_at(0, 2), Some(&Value::Int(110)));
    assert_eq!(
        exec(
            &engine,
            "SELECT count(*) AS count FROM delete_returning_event"
        )
        .value_at(0, 0),
        Some(&Value::Int(1))
    );
    assert_eq!(
        exec(
            &engine,
            "SELECT count(*) AS count FROM delete_returning_action"
        )
        .value_at(0, 0),
        Some(&Value::Int(0))
    );
}

#[test]
fn rule_returning_preserves_update_from_and_delete_using_context() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE update_context_event (id INTEGER PRIMARY KEY, value INTEGER)",
    );
    exec(
        &engine,
        "CREATE TABLE update_context_source (id INTEGER PRIMARY KEY, delta INTEGER)",
    );
    exec(
        &engine,
        "CREATE TABLE update_context_action (id INTEGER PRIMARY KEY, mapped INTEGER)",
    );
    exec(&engine, "INSERT INTO update_context_event VALUES (1, 10)");
    exec(&engine, "INSERT INTO update_context_source VALUES (1, 5)");
    exec(&engine, "INSERT INTO update_context_action VALUES (1, 100)");
    exec(
        &engine,
        "CREATE RULE update_context_provider AS ON UPDATE TO update_context_event DO INSTEAD UPDATE update_context_action SET mapped = NEW.value WHERE id = OLD.id RETURNING id, mapped",
    );

    let updated = exec(
        &engine,
        "UPDATE update_context_event AS event SET value = event.value + source.delta FROM update_context_source AS source WHERE event.id = source.id RETURNING source.delta, old.value, new.value",
    );
    assert_eq!(updated.affected_rows, 1);
    assert_eq!(updated.value_at(0, 0), Some(&Value::Int(5)));
    assert_eq!(updated.value_at(0, 1), Some(&Value::Int(100)));
    assert_eq!(updated.value_at(0, 2), Some(&Value::Int(15)));
    assert_eq!(
        exec(&engine, "SELECT value FROM update_context_event").value_at(0, 0),
        Some(&Value::Int(10))
    );
    assert_eq!(
        exec(&engine, "SELECT mapped FROM update_context_action").value_at(0, 0),
        Some(&Value::Int(15))
    );

    exec(
        &engine,
        "CREATE TABLE delete_context_event (id INTEGER PRIMARY KEY, value INTEGER)",
    );
    exec(
        &engine,
        "CREATE TABLE delete_context_source (id INTEGER PRIMARY KEY, tag TEXT)",
    );
    exec(
        &engine,
        "CREATE TABLE delete_context_action (id INTEGER PRIMARY KEY, mapped INTEGER)",
    );
    exec(&engine, "INSERT INTO delete_context_event VALUES (1, 10)");
    exec(
        &engine,
        "INSERT INTO delete_context_source VALUES (1, 'hit')",
    );
    exec(&engine, "INSERT INTO delete_context_action VALUES (1, 100)");
    exec(
        &engine,
        "CREATE RULE delete_context_provider AS ON DELETE TO delete_context_event DO INSTEAD DELETE FROM delete_context_action WHERE id = OLD.id RETURNING id, mapped",
    );

    let deleted = exec(
        &engine,
        "DELETE FROM delete_context_event AS event USING delete_context_source AS source WHERE event.id = source.id RETURNING source.tag, old.value, new.value",
    );
    assert_eq!(deleted.affected_rows, 1);
    assert_eq!(deleted.value_at(0, 0), Some(&Value::Str("hit".into())));
    assert_eq!(deleted.value_at(0, 1), Some(&Value::Int(100)));
    assert_eq!(deleted.value_at(0, 2), Some(&Value::Null));
    assert_eq!(
        exec(&engine, "SELECT value FROM delete_context_event").value_at(0, 0),
        Some(&Value::Int(10))
    );
    assert!(exec(&engine, "SELECT * FROM delete_context_action")
        .rows
        .is_empty());
}

#[test]
fn rule_returning_retargets_explicit_action_image_aliases() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE alias_returning_event (id INTEGER PRIMARY KEY, value INTEGER)",
    );
    exec(
        &engine,
        "CREATE TABLE alias_returning_action (id INTEGER PRIMARY KEY, mapped INTEGER)",
    );
    exec(&engine, "INSERT INTO alias_returning_event VALUES (1, 10)");
    exec(
        &engine,
        "INSERT INTO alias_returning_action VALUES (1, 100)",
    );
    exec(
        &engine,
        "CREATE RULE alias_returning_provider AS ON UPDATE TO alias_returning_event DO INSTEAD UPDATE alias_returning_action SET mapped = NEW.value + 10 WHERE id = OLD.id RETURNING WITH (OLD AS action_old, NEW AS action_new) id, action_old.mapped + action_new.mapped",
    );
    let result = exec(
        &engine,
        "UPDATE alias_returning_event SET value = value + 1 RETURNING old.value AS old_value, new.value AS new_value, value",
    );
    assert_eq!(result.value_at(0, 0), Some(&Value::Int(200)));
    assert_eq!(result.value_at(0, 1), Some(&Value::Int(42)));
    assert_eq!(result.value_at(0, 2), Some(&Value::Int(121)));
}

#[test]
fn rule_returning_requires_one_active_provider_only_when_instead_can_suppress() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE returning_source (id INTEGER)");
    exec(&engine, "CREATE TABLE returning_log (id INTEGER)");
    exec(
        &engine,
        "CREATE RULE returning_also AS ON INSERT TO returning_source DO ALSO INSERT INTO returning_log VALUES (NEW.id)",
    );
    let ordinary = exec(
        &engine,
        "INSERT INTO returning_source VALUES (1) RETURNING id",
    );
    assert_eq!(ordinary.value_at(0, 0), Some(&Value::Int(1)));

    exec(
        &engine,
        "CREATE RULE returning_conditional_suppress AS ON INSERT TO returning_source WHERE NEW.id < 0 DO INSTEAD NOTHING",
    );
    let missing = engine
        .sql("INSERT INTO returning_source VALUES (2) RETURNING id", &[])
        .expect_err("an INSTEAD rule requires one unconditional provider");
    assert_eq!(missing.sqlstate(), Some("0A000"));
    assert_eq!(
        exec(&engine, "SELECT id FROM returning_source ORDER BY id")
            .rows
            .len(),
        1
    );

    exec(&engine, "CREATE TABLE returning_action_a (id INTEGER)");
    exec(&engine, "CREATE TABLE returning_action_b (id INTEGER)");
    exec(
        &engine,
        "CREATE RULE returning_provider_a AS ON INSERT TO returning_source DO INSTEAD INSERT INTO returning_action_a VALUES (NEW.id) RETURNING id",
    );
    exec(
        &engine,
        "CREATE RULE returning_provider_b AS ON INSERT TO returning_source DO INSTEAD INSERT INTO returning_action_b VALUES (NEW.id) RETURNING id",
    );
    let multiple = engine
        .sql("INSERT INTO returning_source VALUES (3) RETURNING id", &[])
        .expect_err("multiple active providers must fail before action execution");
    assert_eq!(multiple.sqlstate(), Some("0A000"));
    assert!(exec(&engine, "SELECT * FROM returning_action_a")
        .rows
        .is_empty());
    assert!(exec(&engine, "SELECT * FROM returning_action_b")
        .rows
        .is_empty());
}
