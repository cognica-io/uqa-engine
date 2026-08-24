//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18.4 CHECK, FOREIGN KEY, and named NOT NULL lifecycle parity.

use uqa_core::Value;
use uqa_engine::Engine;

fn exec(engine: &Engine, sql: &str) {
    engine.sql(sql, &[]).unwrap_or_else(|error| {
        panic!("SQL failed: {sql}\n{error:?}");
    });
}

fn error(engine: &Engine, sql: &str, state: &str, message: &str) {
    let error = engine.sql(sql, &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some(state), "{sql}: {error}");
    assert!(error.to_string().contains(message), "{sql}: {error}");
}

fn bools(engine: &Engine, name: &str) -> (bool, bool, bool, bool, bool) {
    let result = engine
        .sql(
            &format!(
                "SELECT condeferrable, condeferred, conenforced, convalidated, connoinherit FROM pg_catalog.pg_constraint WHERE conname = '{name}'"
            ),
            &[],
        )
        .unwrap();
    let row = result
        .rows
        .first()
        .unwrap_or_else(|| panic!("missing constraint {name}"));
    let read = |column: &str| match row.get(column) {
        Some(Value::Bool(value)) => *value,
        value => panic!("unexpected {column} for {name}: {value:?}"),
    };
    (
        read("condeferrable"),
        read("condeferred"),
        read("conenforced"),
        read("convalidated"),
        read("connoinherit"),
    )
}

fn constraint_exists(engine: &Engine, name: &str) -> bool {
    !engine
        .sql(
            &format!("SELECT conname FROM pg_catalog.pg_constraint WHERE conname = '{name}'"),
            &[],
        )
        .unwrap()
        .rows
        .is_empty()
}

#[test]
fn check_not_valid_enforces_new_rows_and_validate_is_atomic() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE child (id INTEGER PRIMARY KEY, score INTEGER)",
    );
    exec(&engine, "INSERT INTO child VALUES (1, -1), (2, NULL)");
    exec(
        &engine,
        "ALTER TABLE child ADD CONSTRAINT score_positive CHECK (score > 0) NOT VALID",
    );
    assert_eq!(
        bools(&engine, "score_positive"),
        (false, false, true, false, false)
    );
    error(
        &engine,
        "INSERT INTO child VALUES (3, -2)",
        "23514",
        "violates check constraint \"score_positive\"",
    );
    error(
        &engine,
        "ALTER TABLE child VALIDATE CONSTRAINT score_positive",
        "23514",
        "is violated by some row",
    );
    assert_eq!(
        bools(&engine, "score_positive"),
        (false, false, true, false, false)
    );
    exec(&engine, "UPDATE child SET score = 1 WHERE id = 1");
    exec(
        &engine,
        "ALTER TABLE child VALIDATE CONSTRAINT score_positive",
    );
    assert_eq!(
        bools(&engine, "score_positive"),
        (false, false, true, true, false)
    );
    exec(&engine, "INSERT INTO child VALUES (4, NULL)");
}

#[test]
fn foreign_key_not_valid_and_enforceability_follow_pg18_state_transitions() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE parent (id INTEGER PRIMARY KEY)");
    exec(
        &engine,
        "CREATE TABLE child (id INTEGER PRIMARY KEY, parent_id INTEGER)",
    );
    exec(&engine, "INSERT INTO child VALUES (1, 99)");
    exec(
        &engine,
        "ALTER TABLE child ADD CONSTRAINT child_parent_fk FOREIGN KEY (parent_id) REFERENCES parent(id) NOT VALID",
    );
    assert_eq!(
        bools(&engine, "child_parent_fk"),
        (false, false, true, false, true)
    );
    error(
        &engine,
        "INSERT INTO child VALUES (2, 100)",
        "23503",
        "violates foreign key constraint \"child_parent_fk\"",
    );
    error(
        &engine,
        "ALTER TABLE child VALIDATE CONSTRAINT child_parent_fk",
        "23503",
        "violates foreign key constraint \"child_parent_fk\"",
    );
    exec(&engine, "INSERT INTO parent VALUES (99)");
    exec(
        &engine,
        "ALTER TABLE child VALIDATE CONSTRAINT child_parent_fk",
    );
    assert_eq!(
        bools(&engine, "child_parent_fk"),
        (false, false, true, true, true)
    );

    exec(
        &engine,
        "ALTER TABLE child ALTER CONSTRAINT child_parent_fk NOT ENFORCED",
    );
    assert_eq!(
        bools(&engine, "child_parent_fk"),
        (false, false, false, false, true)
    );
    exec(&engine, "INSERT INTO child VALUES (3, 12345)");
    error(
        &engine,
        "ALTER TABLE child ALTER CONSTRAINT child_parent_fk ENFORCED",
        "23503",
        "violates foreign key constraint \"child_parent_fk\"",
    );
    assert_eq!(
        bools(&engine, "child_parent_fk"),
        (false, false, false, false, true)
    );
    exec(&engine, "DELETE FROM child WHERE id = 3");
    exec(
        &engine,
        "ALTER TABLE child ALTER CONSTRAINT child_parent_fk ENFORCED",
    );
    assert_eq!(
        bools(&engine, "child_parent_fk"),
        (false, false, true, true, true)
    );
}

#[test]
fn initially_deferred_foreign_key_checks_final_commit_state_and_savepoints() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE parent (id INTEGER PRIMARY KEY)");
    exec(
        &engine,
        "CREATE TABLE child (id INTEGER PRIMARY KEY, parent_id INTEGER, CONSTRAINT child_parent_fk FOREIGN KEY (parent_id) REFERENCES parent(id) DEFERRABLE INITIALLY DEFERRED)",
    );
    exec(&engine, "INSERT INTO parent VALUES (99)");
    exec(&engine, "INSERT INTO child VALUES (1, 99)");
    assert_eq!(
        bools(&engine, "child_parent_fk"),
        (true, true, true, true, true)
    );
    exec(&engine, "BEGIN");
    exec(&engine, "INSERT INTO child VALUES (4, 777)");
    exec(&engine, "INSERT INTO parent VALUES (777)");
    exec(&engine, "COMMIT");
    exec(&engine, "BEGIN");
    exec(&engine, "INSERT INTO child VALUES (5, 888)");
    error(
        &engine,
        "COMMIT",
        "23503",
        "violates foreign key constraint \"child_parent_fk\"",
    );
    exec(&engine, "BEGIN");
    exec(&engine, "SAVEPOINT before_invalid_child");
    exec(&engine, "INSERT INTO child VALUES (6, 999)");
    exec(&engine, "ROLLBACK TO SAVEPOINT before_invalid_child");
    exec(&engine, "COMMIT");
    exec(&engine, "BEGIN");
    exec(&engine, "DELETE FROM parent WHERE id = 99");
    exec(&engine, "INSERT INTO parent VALUES (99)");
    exec(&engine, "COMMIT");
    exec(&engine, "BEGIN");
    exec(&engine, "DELETE FROM parent WHERE id = 99");
    error(
        &engine,
        "COMMIT",
        "23503",
        "violates foreign key constraint \"child_parent_fk\"",
    );
    exec(
        &engine,
        "ALTER TABLE child ALTER CONSTRAINT child_parent_fk NOT DEFERRABLE INITIALLY IMMEDIATE",
    );
    assert_eq!(
        bools(&engine, "child_parent_fk"),
        (false, false, true, true, true)
    );
}

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
