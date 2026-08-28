//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn named_not_null_not_valid_validate_inherit_and_drop_are_durable_metadata() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE values_table (id INTEGER PRIMARY KEY, value INTEGER)",
    );
    exec(&engine, "INSERT INTO values_table VALUES (1, NULL)");
    exec(
        &engine,
        "ALTER TABLE values_table ADD CONSTRAINT value_nn NOT NULL value NOT VALID",
    );
    assert_eq!(
        bools(&engine, "value_nn"),
        (false, false, true, false, false)
    );
    error(
        &engine,
        "INSERT INTO values_table VALUES (2, NULL)",
        "23502",
        "violates not-null constraint",
    );
    error(
        &engine,
        "ALTER TABLE values_table VALIDATE CONSTRAINT value_nn",
        "23502",
        "contains null values",
    );
    exec(&engine, "UPDATE values_table SET value = 7 WHERE id = 1");
    exec(
        &engine,
        "ALTER TABLE values_table VALIDATE CONSTRAINT value_nn",
    );
    exec(
        &engine,
        "ALTER TABLE values_table ALTER CONSTRAINT value_nn NO INHERIT",
    );
    assert_eq!(bools(&engine, "value_nn"), (false, false, true, true, true));
    exec(
        &engine,
        "ALTER TABLE values_table ALTER CONSTRAINT value_nn INHERIT",
    );
    assert_eq!(
        bools(&engine, "value_nn"),
        (false, false, true, true, false)
    );
    exec(&engine, "ALTER TABLE values_table DROP CONSTRAINT value_nn");
    assert!(!constraint_exists(&engine, "value_nn"));
    let result = engine
        .sql(
            "SELECT attnotnull FROM pg_catalog.pg_attribute WHERE attrelid = 'values_table'::regclass AND attname = 'value'",
            &[],
        )
        .unwrap();
    assert_eq!(
        result.rows.first().and_then(|row| row.get("attnotnull")),
        Some(&Value::Bool(false))
    );
    exec(&engine, "INSERT INTO values_table VALUES (2, NULL)");

    exec(&engine, "CREATE TABLE keyed (id INTEGER PRIMARY KEY)");
    error(
        &engine,
        "ALTER TABLE keyed DROP CONSTRAINT keyed_id_not_null",
        "42P16",
        "is in a primary key",
    );
    assert!(constraint_exists(&engine, "keyed_id_not_null"));
}

#[test]
fn dependency_drops_remove_owned_constraints_and_bound_foreign_keys_only() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE parent (id INTEGER CONSTRAINT parent_id_key UNIQUE)",
    );
    exec(
        &engine,
        "CREATE TABLE child (id INTEGER PRIMARY KEY, parent_id INTEGER CONSTRAINT child_parent_fk REFERENCES parent(id), score INTEGER CONSTRAINT score_ck CHECK (score > 0), label TEXT CONSTRAINT label_nn NOT NULL)",
    );
    error(
        &engine,
        "ALTER TABLE parent DROP CONSTRAINT parent_id_key RESTRICT",
        "2BP01",
        "other objects depend on it",
    );
    assert!(constraint_exists(&engine, "parent_id_key"));
    assert!(constraint_exists(&engine, "child_parent_fk"));
    exec(
        &engine,
        "ALTER TABLE parent DROP CONSTRAINT parent_id_key CASCADE",
    );
    assert!(!constraint_exists(&engine, "parent_id_key"));
    assert!(!constraint_exists(&engine, "child_parent_fk"));
    assert!(engine.has_table("child").unwrap());
    let result = engine
        .sql(
            "SELECT to_regclass('child') IS NOT NULL AS child_exists",
            &[],
        )
        .unwrap();
    assert_eq!(
        result.rows.first().and_then(|row| row.get("child_exists")),
        Some(&Value::Bool(true))
    );

    exec(
        &engine,
        "ALTER TABLE parent ADD CONSTRAINT parent_id_key UNIQUE (id)",
    );
    exec(
        &engine,
        "ALTER TABLE child ADD CONSTRAINT child_parent_fk FOREIGN KEY (parent_id) REFERENCES parent(id)",
    );
    error(
        &engine,
        "ALTER TABLE parent DROP COLUMN id RESTRICT",
        "2BP01",
        "other objects depend on it",
    );
    assert!(constraint_exists(&engine, "child_parent_fk"));
    exec(&engine, "ALTER TABLE parent DROP COLUMN id CASCADE");
    assert!(!constraint_exists(&engine, "child_parent_fk"));

    exec(&engine, "ALTER TABLE child DROP COLUMN score");
    assert!(!constraint_exists(&engine, "score_ck"));
    exec(&engine, "ALTER TABLE child DROP CONSTRAINT label_nn");
    assert!(!constraint_exists(&engine, "label_nn"));

    exec(
        &engine,
        "CREATE TABLE self_ref (id INTEGER CONSTRAINT self_key UNIQUE, parent_id INTEGER CONSTRAINT self_fk REFERENCES self_ref(id))",
    );
    exec(
        &engine,
        "ALTER TABLE self_ref DROP CONSTRAINT self_key CASCADE",
    );
    assert!(!constraint_exists(&engine, "self_key"));
    assert!(!constraint_exists(&engine, "self_fk"));
}

#[test]
fn multi_action_failure_rolls_back_every_constraint_catalog_change() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE atomic_table (value INTEGER)");
    exec(&engine, "INSERT INTO atomic_table VALUES (-1)");
    error(
        &engine,
        "ALTER TABLE atomic_table ADD CONSTRAINT positive CHECK (value > 0) NOT VALID, VALIDATE CONSTRAINT positive",
        "23514",
        "is violated by some row",
    );
    assert!(!constraint_exists(&engine, "positive"));
    exec(&engine, "INSERT INTO atomic_table VALUES (-2)");
    error(
        &engine,
        "ALTER TABLE atomic_table ADD CONSTRAINT duplicate CHECK (value <> 0) NOT VALID, ADD CONSTRAINT duplicate CHECK (value <> 1) NOT VALID",
        "42710",
        "already exists",
    );
    assert!(!constraint_exists(&engine, "duplicate"));
}

#[test]
fn invalid_alter_forms_fail_without_mutating_constraint_state() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE parent (id INTEGER PRIMARY KEY)");
    exec(
        &engine,
        "CREATE TABLE child (value INTEGER CONSTRAINT value_ck CHECK (value > 0), parent_id INTEGER CONSTRAINT parent_fk REFERENCES parent(id))",
    );
    error(
        &engine,
        "ALTER TABLE child ALTER CONSTRAINT value_ck NOT ENFORCED",
        "42809",
        "cannot alter enforceability",
    );
    error(
        &engine,
        "ALTER TABLE child ALTER CONSTRAINT value_ck DEFERRABLE",
        "42809",
        "is not a foreign key constraint",
    );
    error(
        &engine,
        "ALTER TABLE child ALTER CONSTRAINT parent_fk NO INHERIT",
        "42809",
        "is not a not-null constraint",
    );
    error(
        &engine,
        "ALTER TABLE child ALTER CONSTRAINT parent_fk NOT VALID",
        "0A000",
        "constraints cannot be altered to be NOT VALID",
    );
    error(
        &engine,
        "ALTER TABLE child VALIDATE CONSTRAINT absent",
        "42704",
        "does not exist",
    );
    assert_eq!(
        bools(&engine, "value_ck"),
        (false, false, true, true, false)
    );
    assert_eq!(
        bools(&engine, "parent_fk"),
        (false, false, true, true, true)
    );
}

#[test]
fn persistent_reopen_restores_validation_enforcement_deferral_and_inheritance() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("constraint-lifecycle.sqlite");
    {
        let engine = Engine::open(&database).unwrap();
        exec(&engine, "CREATE TABLE parent (id INTEGER PRIMARY KEY)");
        exec(
            &engine,
            "CREATE TABLE child (id INTEGER PRIMARY KEY, parent_id INTEGER, score INTEGER, label TEXT)",
        );
        exec(
            &engine,
            "ALTER TABLE child ADD CONSTRAINT score_ck CHECK (score > 0) NOT VALID",
        );
        exec(
            &engine,
            "ALTER TABLE child ADD CONSTRAINT parent_fk FOREIGN KEY (parent_id) REFERENCES parent(id) NOT ENFORCED DEFERRABLE INITIALLY DEFERRED",
        );
        exec(
            &engine,
            "ALTER TABLE child ADD CONSTRAINT label_nn NOT NULL label NOT VALID NO INHERIT",
        );
        exec(
            &engine,
            "CREATE TABLE active_child (id INTEGER PRIMARY KEY, parent_id INTEGER, CONSTRAINT active_fk FOREIGN KEY (parent_id) REFERENCES parent(id) NOT VALID DEFERRABLE INITIALLY DEFERRED)",
        );
    }
    let reopened = Engine::open(&database).unwrap();
    assert_eq!(
        bools(&reopened, "score_ck"),
        (false, false, true, false, false)
    );
    assert_eq!(
        bools(&reopened, "parent_fk"),
        (true, true, false, false, true)
    );
    assert_eq!(
        bools(&reopened, "label_nn"),
        (false, false, true, false, true)
    );
    assert_eq!(
        bools(&reopened, "active_fk"),
        (true, true, true, false, true)
    );
    error(
        &reopened,
        "INSERT INTO child VALUES (1, 999, -1, 'x')",
        "23514",
        "violates check constraint \"score_ck\"",
    );
    exec(&reopened, "INSERT INTO child VALUES (1, 999, 1, 'x')");
    exec(&reopened, "BEGIN");
    exec(&reopened, "INSERT INTO active_child VALUES (1, 1001)");
    exec(&reopened, "INSERT INTO parent VALUES (1001)");
    exec(&reopened, "COMMIT");
}

#[test]
fn failed_foreign_key_create_is_atomic_across_persistent_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("failed-foreign-key-create.sqlite");
    {
        let engine = Engine::open(&database).unwrap();
        exec(&engine, "CREATE TABLE text_parent (id TEXT PRIMARY KEY)");
        let create_error = engine
            .transaction(|transaction| {
                transaction.sql(
                    "CREATE TABLE rejected_child (parent_id INTEGER CONSTRAINT rejected_child_fk REFERENCES text_parent)",
                    &[],
                )?;
                Ok(())
            })
            .unwrap_err();
        assert_eq!(create_error.sqlstate(), Some("42804"));
        assert!(create_error.to_string().contains("incompatible types"));
        assert!(!engine.has_table("rejected_child").unwrap());
        assert!(!constraint_exists(&engine, "rejected_child_fk"));
    }

    let reopened = Engine::open(&database).unwrap();
    assert!(reopened.has_table("text_parent").unwrap());
    assert!(!reopened.has_table("rejected_child").unwrap());
    assert!(!constraint_exists(&reopened, "rejected_child_fk"));
}

#[test]
fn create_table_not_valid_state_and_check_null_semantics_match_pg18() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE parent (id INTEGER PRIMARY KEY)");
    exec(
        &engine,
        "CREATE TABLE created (value INTEGER, parent_id INTEGER, CONSTRAINT created_ck CHECK (value > 0) NOT VALID, CONSTRAINT created_fk FOREIGN KEY (parent_id) REFERENCES parent(id) NOT VALID)",
    );
    assert_eq!(
        bools(&engine, "created_ck"),
        (false, false, true, false, false)
    );
    assert_eq!(
        bools(&engine, "created_fk"),
        (false, false, true, false, true)
    );
    exec(&engine, "INSERT INTO created VALUES (NULL, NULL)");
}
