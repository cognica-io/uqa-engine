//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18.4 CHECK, FOREIGN KEY, and named NOT NULL lifecycle parity.

use std::sync::{Arc, Weak};

use uqa_core::Value;
use uqa_engine::Engine;
use uqa_sql::SQLError;

#[path = "pg18_constraint_lifecycle/persistence.rs"]
mod persistence;

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

fn deferred_event_engine() -> Engine {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE root (id INTEGER PRIMARY KEY)");
    exec(&engine, "CREATE TABLE parent_a (id INTEGER PRIMARY KEY)");
    exec(&engine, "CREATE TABLE parent_b (id INTEGER PRIMARY KEY)");
    exec(
        &engine,
        "CREATE TABLE child (id INTEGER PRIMARY KEY, a_id INTEGER, b_id INTEGER, marker INTEGER, CONSTRAINT child_a_fk FOREIGN KEY (a_id) REFERENCES parent_a(id) DEFERRABLE INITIALLY DEFERRED)",
    );
    exec(&engine, "INSERT INTO parent_a VALUES (1)");
    exec(&engine, "INSERT INTO child VALUES (1, 1, 999, 0)");
    exec(
        &engine,
        "ALTER TABLE child ADD CONSTRAINT child_b_fk FOREIGN KEY (b_id) REFERENCES parent_b(id) DEFERRABLE INITIALLY IMMEDIATE NOT VALID",
    );
    engine
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
fn recreated_foreign_key_triggers_reset_named_modes_but_preserve_all_mode() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE parent (id INTEGER PRIMARY KEY)");
    exec(
        &engine,
        "CREATE TABLE child (id INTEGER PRIMARY KEY, parent_id INTEGER, CONSTRAINT child_parent_fk FOREIGN KEY (parent_id) REFERENCES parent(id) DEFERRABLE INITIALLY IMMEDIATE)",
    );

    exec(&engine, "BEGIN");
    exec(&engine, "SET CONSTRAINTS child_parent_fk DEFERRED");
    exec(
        &engine,
        "ALTER TABLE child ALTER CONSTRAINT child_parent_fk NOT ENFORCED",
    );
    exec(
        &engine,
        "ALTER TABLE child ALTER CONSTRAINT child_parent_fk ENFORCED",
    );
    exec(&engine, "SAVEPOINT recreated_trigger_is_immediate");
    error(
        &engine,
        "INSERT INTO child VALUES (1, 101)",
        "23503",
        "violates foreign key constraint \"child_parent_fk\"",
    );
    exec(
        &engine,
        "ROLLBACK TO SAVEPOINT recreated_trigger_is_immediate",
    );
    exec(&engine, "ROLLBACK");

    exec(&engine, "BEGIN");
    exec(&engine, "SET CONSTRAINTS ALL DEFERRED");
    exec(
        &engine,
        "ALTER TABLE child ALTER CONSTRAINT child_parent_fk NOT ENFORCED",
    );
    exec(
        &engine,
        "ALTER TABLE child ALTER CONSTRAINT child_parent_fk ENFORCED",
    );
    exec(&engine, "INSERT INTO child VALUES (2, 202)");
    exec(&engine, "INSERT INTO parent VALUES (202)");
    exec(&engine, "COMMIT");
}

#[test]
fn omitted_foreign_key_columns_infer_primary_keys_and_check_types() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE parent (id BIGINT PRIMARY KEY)");
    exec(
        &engine,
        "CREATE TABLE table_child (id INTEGER PRIMARY KEY, parent_id INTEGER, CONSTRAINT table_child_parent_fk FOREIGN KEY (parent_id) REFERENCES parent)",
    );
    exec(
        &engine,
        "CREATE TABLE column_child (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES parent)",
    );
    exec(
        &engine,
        "CREATE TABLE altered_child (id INTEGER PRIMARY KEY, parent_id INTEGER)",
    );
    exec(
        &engine,
        "ALTER TABLE altered_child ADD CONSTRAINT altered_child_parent_fk FOREIGN KEY (parent_id) REFERENCES parent",
    );

    for table in ["table_child", "column_child", "altered_child"] {
        let foreign_keys = engine.foreign_keys(table).unwrap();
        assert_eq!(foreign_keys.len(), 1, "{table}");
        assert_eq!(foreign_keys[0].ref_columns, ["id"], "{table}");
    }

    exec(&engine, "INSERT INTO parent VALUES (7)");
    exec(&engine, "INSERT INTO table_child VALUES (1, 7)");
    exec(&engine, "INSERT INTO column_child VALUES (1, 7)");
    exec(&engine, "INSERT INTO altered_child VALUES (1, 7)");
    error(
        &engine,
        "INSERT INTO table_child VALUES (2, 8)",
        "23503",
        "violates foreign key constraint \"table_child_parent_fk\"",
    );

    exec(&engine, "CREATE TABLE no_key (id INTEGER)");
    error(
        &engine,
        "CREATE TABLE no_key_child (parent_id INTEGER REFERENCES no_key)",
        "42704",
        "there is no primary key for referenced table",
    );
    exec(
        &engine,
        "CREATE TABLE composite_parent (tenant_id INTEGER, id INTEGER, PRIMARY KEY (tenant_id, id))",
    );
    error(
        &engine,
        "CREATE TABLE composite_child (parent_id INTEGER REFERENCES composite_parent)",
        "42830",
        "number of referencing and referenced columns for foreign key disagree",
    );
    error(
        &engine,
        "CREATE TABLE duplicate_ref_child (tenant_id INTEGER, id INTEGER, FOREIGN KEY (tenant_id, id) REFERENCES composite_parent(tenant_id, tenant_id))",
        "42830",
        "there is no unique constraint matching given keys",
    );

    exec(
        &engine,
        "CREATE TABLE numeric_parent (id NUMERIC PRIMARY KEY)",
    );
    error(
        &engine,
        "CREATE TABLE real_child (parent_id REAL REFERENCES numeric_parent)",
        "42804",
        "incompatible types",
    );
    exec(&engine, "CREATE TABLE real_parent (id REAL PRIMARY KEY)");
    exec(
        &engine,
        "CREATE TABLE numeric_child (parent_id NUMERIC REFERENCES real_parent)",
    );
    exec(&engine, "INSERT INTO real_parent VALUES (1.25)");
    exec(&engine, "INSERT INTO numeric_child VALUES (1.25)");
}

#[test]
fn temporal_cross_type_foreign_keys_preserve_values_and_referential_actions() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE date_parent (id DATE PRIMARY KEY)");
    exec(
        &engine,
        "CREATE TABLE timestamp_child (id INTEGER PRIMARY KEY, parent_id TIMESTAMP REFERENCES date_parent ON DELETE SET NULL)",
    );
    exec(
        &engine,
        "INSERT INTO date_parent VALUES (DATE '2024-01-01')",
    );
    exec(
        &engine,
        "INSERT INTO timestamp_child VALUES (1, TIMESTAMP '2024-01-01 00:00:00')",
    );
    error(
        &engine,
        "INSERT INTO timestamp_child VALUES (2, TIMESTAMP '2024-01-01 12:00:00')",
        "23503",
        "violates foreign key constraint",
    );
    exec(&engine, "DELETE FROM date_parent");
    let row = engine
        .sql("SELECT parent_id FROM timestamp_child WHERE id = 1", &[])
        .unwrap();
    assert_eq!(row.rows[0]["parent_id"], Value::Null);

    exec(
        &engine,
        "CREATE TABLE timestamp_parent (id TIMESTAMP PRIMARY KEY)",
    );
    exec(
        &engine,
        "CREATE TABLE date_child (parent_id DATE REFERENCES timestamp_parent ON UPDATE CASCADE)",
    );
    exec(
        &engine,
        "INSERT INTO timestamp_parent VALUES (TIMESTAMP '2024-02-01 00:00:00')",
    );
    exec(&engine, "INSERT INTO date_child VALUES (DATE '2024-02-01')");
    exec(
        &engine,
        "UPDATE timestamp_parent SET id = TIMESTAMP '2024-02-02 00:00:00'",
    );
    let row = engine
        .sql(
            "SELECT parent_id = DATE '2024-02-02' AS updated FROM date_child",
            &[],
        )
        .unwrap();
    assert_eq!(row.rows[0]["updated"], Value::Bool(true));

    exec(
        &engine,
        "CREATE TABLE deferred_date_parent (id DATE PRIMARY KEY)",
    );
    exec(
        &engine,
        "CREATE TABLE deferred_timestamp_child (id INTEGER PRIMARY KEY, parent_id TIMESTAMP, CONSTRAINT deferred_timestamp_child_fk FOREIGN KEY (parent_id) REFERENCES deferred_date_parent(id) DEFERRABLE INITIALLY DEFERRED)",
    );
    exec(
        &engine,
        "INSERT INTO deferred_date_parent VALUES (DATE '2024-03-01'), (DATE '2024-03-02'), (DATE '2024-03-03')",
    );
    exec(&engine, "BEGIN");
    exec(
        &engine,
        "INSERT INTO deferred_timestamp_child VALUES (1, TIMESTAMP '2024-03-01 00:00:00'), (2, TIMESTAMP '2024-03-02 00:00:00'), (3, TIMESTAMP '2024-03-03 00:00:00')",
    );
    exec(&engine, "COMMIT");
    exec(&engine, "BEGIN");
    exec(
        &engine,
        "INSERT INTO deferred_timestamp_child VALUES (4, TIMESTAMP '2024-03-04 00:00:00')",
    );
    error(
        &engine,
        "COMMIT",
        "23503",
        "violates foreign key constraint",
    );
    let rolled_back = engine
        .sql("SELECT id FROM deferred_timestamp_child WHERE id = 4", &[])
        .unwrap();
    assert!(rolled_back.rows.is_empty());
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

    let nested_error = engine
        .transaction(|outer| {
            outer.transaction(|inner| {
                inner.sql("INSERT INTO child VALUES (7, 7007)", &[])?;
                Ok(())
            })
        })
        .unwrap_err();
    assert_eq!(nested_error.sqlstate(), Some("23503"));
    assert!(nested_error
        .to_string()
        .contains("violates foreign key constraint \"child_parent_fk\""));
    assert!(engine
        .sql("SELECT id FROM child WHERE id = 7", &[])
        .unwrap()
        .rows
        .is_empty());

    engine
        .transaction(|outer| {
            outer.transaction(|inner| {
                inner.sql("INSERT INTO child VALUES (8, 8008)", &[])?;
                Ok(())
            })?;
            outer.sql("INSERT INTO parent VALUES (8008)", &[])?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        engine
            .sql("SELECT parent_id FROM child WHERE id = 8", &[])
            .unwrap()
            .rows
            .len(),
        1
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
fn set_constraints_changes_foreign_key_modes_retroactively_and_transactionally() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE parent (id INTEGER PRIMARY KEY)");
    exec(
        &engine,
        "CREATE TABLE child (id INTEGER PRIMARY KEY, parent_id INTEGER, CONSTRAINT child_parent_fk FOREIGN KEY (parent_id) REFERENCES parent(id) DEFERRABLE INITIALLY IMMEDIATE)",
    );

    exec(&engine, "BEGIN");
    exec(&engine, "SET CONSTRAINTS child_parent_fk DEFERRED");
    exec(&engine, "INSERT INTO child VALUES (1, 101)");
    exec(&engine, "SAVEPOINT before_retroactive_check");
    error(
        &engine,
        "SET CONSTRAINTS child_parent_fk IMMEDIATE",
        "23503",
        "violates foreign key constraint \"child_parent_fk\"",
    );
    exec(&engine, "ROLLBACK TO SAVEPOINT before_retroactive_check");
    exec(&engine, "INSERT INTO parent VALUES (101)");
    exec(&engine, "SET CONSTRAINTS child_parent_fk IMMEDIATE");

    exec(&engine, "SAVEPOINT before_mode_change");
    exec(&engine, "SET CONSTRAINTS child_parent_fk DEFERRED");
    exec(&engine, "ROLLBACK TO SAVEPOINT before_mode_change");
    error(
        &engine,
        "INSERT INTO child VALUES (2, 202)",
        "23503",
        "violates foreign key constraint \"child_parent_fk\"",
    );
    exec(&engine, "ROLLBACK TO SAVEPOINT before_mode_change");

    exec(&engine, "SET CONSTRAINTS child_parent_fk DEFERRED");
    exec(&engine, "DELETE FROM parent WHERE id = 101");
    exec(&engine, "SAVEPOINT before_parent_check");
    error(
        &engine,
        "SET CONSTRAINTS child_parent_fk IMMEDIATE",
        "23503",
        "violates foreign key constraint \"child_parent_fk\"",
    );
    exec(&engine, "ROLLBACK TO SAVEPOINT before_parent_check");
    exec(&engine, "INSERT INTO parent VALUES (101)");
    exec(&engine, "SET CONSTRAINTS child_parent_fk IMMEDIATE");
    exec(&engine, "COMMIT");

    exec(&engine, "BEGIN");
    exec(&engine, "SAVEPOINT initial_mode");
    error(
        &engine,
        "INSERT INTO child VALUES (3, 303)",
        "23503",
        "violates foreign key constraint \"child_parent_fk\"",
    );
    exec(&engine, "ROLLBACK TO SAVEPOINT initial_mode");
    exec(&engine, "ROLLBACK");
    assert_eq!(
        engine
            .sql("SELECT id FROM child ORDER BY id", &[])
            .unwrap()
            .rows
            .len(),
        1
    );

    engine
        .transaction(|outer| {
            outer.sql("SET CONSTRAINTS child_parent_fk DEFERRED", &[])?;
            outer.transaction(|inner| {
                inner.sql("INSERT INTO child VALUES (4, 404)", &[])?;
                Ok(())
            })?;
            outer.sql("INSERT INTO parent VALUES (404)", &[])?;
            Ok(())
        })
        .unwrap();

    exec(&engine, "BEGIN");
    exec(&engine, "SET CONSTRAINTS child_parent_fk DEFERRED");
    exec(&engine, "INSERT INTO child VALUES (5, 505)");
    exec(&engine, "ALTER TABLE child RENAME TO renamed_child");
    exec(&engine, "INSERT INTO renamed_child VALUES (6, 606)");
    exec(&engine, "INSERT INTO parent VALUES (505), (606)");
    exec(&engine, "COMMIT");

    exec(&engine, "BEGIN");
    exec(&engine, "SET CONSTRAINTS child_parent_fk DEFERRED");
    exec(&engine, "UPDATE parent SET id = 406 WHERE id = 404");
    exec(&engine, "SAVEPOINT before_parent_update_check");
    error(
        &engine,
        "SET CONSTRAINTS child_parent_fk IMMEDIATE",
        "23503",
        "violates foreign key constraint \"child_parent_fk\"",
    );
    exec(&engine, "ROLLBACK TO SAVEPOINT before_parent_update_check");
    exec(&engine, "INSERT INTO parent VALUES (404)");
    exec(&engine, "SET CONSTRAINTS child_parent_fk IMMEDIATE");
    exec(&engine, "COMMIT");
}

#[test]
fn set_constraints_all_object_lifecycle_and_nested_execution_match_postgresql() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE parent (id INTEGER PRIMARY KEY)");
    exec(
        &engine,
        "CREATE TABLE child (id INTEGER PRIMARY KEY, parent_id INTEGER, CONSTRAINT positive CHECK (id > 0), CONSTRAINT child_parent_fk FOREIGN KEY (parent_id) REFERENCES parent(id) DEFERRABLE INITIALLY IMMEDIATE)",
    );

    engine.take_sql_notices();
    error(
        &engine,
        "SET CONSTRAINTS positive DEFERRED",
        "42809",
        "constraint \"positive\" is not deferrable",
    );
    assert!(engine.take_sql_notices().iter().any(|(level, message)| {
        level == "WARNING" && message == "SET CONSTRAINTS can only be used in transaction blocks"
    }));

    exec(&engine, "BEGIN");
    exec(&engine, "SET CONSTRAINTS positive IMMEDIATE");
    exec(&engine, "COMMIT");

    exec(&engine, "BEGIN");
    exec(&engine, "SET CONSTRAINTS child_parent_fk DEFERRED");
    exec(&engine, "ALTER TABLE child DROP CONSTRAINT child_parent_fk");
    exec(
        &engine,
        "ALTER TABLE child ADD CONSTRAINT child_parent_fk FOREIGN KEY (parent_id) REFERENCES parent(id) DEFERRABLE INITIALLY IMMEDIATE",
    );
    exec(&engine, "SAVEPOINT replacement_is_immediate");
    error(
        &engine,
        "INSERT INTO child VALUES (1, 101)",
        "23503",
        "violates foreign key constraint \"child_parent_fk\"",
    );
    exec(&engine, "ROLLBACK TO SAVEPOINT replacement_is_immediate");
    exec(&engine, "ROLLBACK");

    exec(&engine, "BEGIN");
    exec(&engine, "SET CONSTRAINTS ALL DEFERRED");
    exec(
        &engine,
        "CREATE TABLE later_child (id INTEGER PRIMARY KEY, parent_id INTEGER, CONSTRAINT later_parent_fk FOREIGN KEY (parent_id) REFERENCES parent(id) DEFERRABLE INITIALLY IMMEDIATE)",
    );
    exec(&engine, "INSERT INTO later_child VALUES (2, 202)");
    exec(&engine, "INSERT INTO parent VALUES (202)");
    exec(&engine, "COMMIT");

    exec(&engine, "BEGIN");
    exec(&engine, "SET CONSTRAINTS child_parent_fk DEFERRED");
    exec(&engine, "SET CONSTRAINTS ALL IMMEDIATE");
    exec(&engine, "SAVEPOINT all_clears_named_modes");
    error(
        &engine,
        "INSERT INTO child VALUES (3, 303)",
        "23503",
        "violates foreign key constraint \"child_parent_fk\"",
    );
    exec(&engine, "ROLLBACK TO SAVEPOINT all_clears_named_modes");
    exec(&engine, "ROLLBACK");

    exec(
        &engine,
        "CREATE PROCEDURE insert_child_before_parent() LANGUAGE plpgsql AS $$ BEGIN EXECUTE 'SET CONSTRAINTS child_parent_fk DEFERRED'; INSERT INTO child VALUES (4, 404); INSERT INTO parent VALUES (404); END $$",
    );
    engine.take_sql_notices();
    exec(&engine, "CALL insert_child_before_parent()");
    assert!(!engine.take_sql_notices().iter().any(|(level, message)| {
        level == "WARNING" && message == "SET CONSTRAINTS can only be used in transaction blocks"
    }));

    exec(
        &engine,
        "CREATE PROCEDURE catch_retroactive_violation() LANGUAGE plpgsql AS $$ BEGIN BEGIN SET CONSTRAINTS child_parent_fk IMMEDIATE; EXCEPTION WHEN foreign_key_violation THEN NULL; END; END $$",
    );
    exec(&engine, "BEGIN");
    exec(&engine, "SET CONSTRAINTS child_parent_fk DEFERRED");
    exec(&engine, "INSERT INTO child VALUES (5, 505)");
    exec(&engine, "CALL catch_retroactive_violation()");
    error(
        &engine,
        "COMMIT",
        "23503",
        "violates foreign key constraint \"child_parent_fk\"",
    );
    assert!(engine
        .sql("SELECT id FROM child WHERE id = 5", &[])
        .unwrap()
        .rows
        .is_empty());
}

#[test]
fn set_constraints_drop_column_replacement_uses_the_new_constraint_mode() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE parent (id INTEGER PRIMARY KEY)");
    exec(
        &engine,
        "CREATE TABLE child (id INTEGER PRIMARY KEY, parent_id INTEGER, CONSTRAINT child_parent_fk FOREIGN KEY (parent_id) REFERENCES parent(id) DEFERRABLE INITIALLY IMMEDIATE)",
    );

    exec(&engine, "BEGIN");
    exec(&engine, "SET CONSTRAINTS child_parent_fk DEFERRED");
    exec(&engine, "ALTER TABLE child DROP COLUMN parent_id");
    exec(&engine, "ALTER TABLE child ADD COLUMN parent_id INTEGER");
    exec(
        &engine,
        "ALTER TABLE child ADD CONSTRAINT child_parent_fk FOREIGN KEY (parent_id) REFERENCES parent(id) DEFERRABLE INITIALLY IMMEDIATE",
    );
    exec(&engine, "SAVEPOINT replacement_is_immediate");
    error(
        &engine,
        "INSERT INTO child VALUES (1, 101)",
        "23503",
        "violates foreign key constraint \"child_parent_fk\"",
    );
    exec(&engine, "ROLLBACK TO SAVEPOINT replacement_is_immediate");
    exec(&engine, "ROLLBACK");
}

#[test]
fn deferred_events_retain_their_constraint_identity_and_block_deferrability_changes() {
    let engine = deferred_event_engine();
    exec(&engine, "BEGIN");
    exec(&engine, "SET CONSTRAINTS child_b_fk DEFERRED");
    exec(&engine, "UPDATE child SET marker = 1 WHERE id = 1");
    exec(&engine, "COMMIT");

    exec(&engine, "BEGIN");
    exec(&engine, "SET CONSTRAINTS child_a_fk DEFERRED");
    exec(&engine, "INSERT INTO child VALUES (2, 202, NULL, 0)");
    error(
        &engine,
        "ALTER TABLE child ALTER CONSTRAINT child_a_fk NOT DEFERRABLE",
        "55006",
        "pending trigger events",
    );
    exec(&engine, "ROLLBACK");
    assert!(engine
        .sql("SELECT id FROM child WHERE id = 2", &[])
        .unwrap()
        .rows
        .is_empty());

    exec(&engine, "BEGIN");
    exec(&engine, "SET CONSTRAINTS child_a_fk DEFERRED");
    exec(&engine, "INSERT INTO child VALUES (3, 303, NULL, 0)");
    error(
        &engine,
        "ALTER TABLE child ALTER CONSTRAINT child_a_fk NOT ENFORCED",
        "55006",
        "pending trigger events",
    );
    exec(&engine, "ROLLBACK");
}

#[test]
fn deferred_events_follow_row_rewrites_and_survive_constraint_changes() {
    let engine = deferred_event_engine();
    exec(&engine, "BEGIN");
    exec(&engine, "SET CONSTRAINTS child_a_fk DEFERRED");
    exec(&engine, "DELETE FROM parent_a WHERE id = 1");
    error(
        &engine,
        "ALTER TABLE child DROP CONSTRAINT child_a_fk",
        "55006",
        "cannot ALTER TABLE \"parent_a\" because it has pending trigger events",
    );
    exec(&engine, "ROLLBACK");

    exec(&engine, "BEGIN");
    exec(&engine, "SET CONSTRAINTS child_a_fk DEFERRED");
    exec(&engine, "DELETE FROM parent_a WHERE id = 1");
    exec(
        &engine,
        "ALTER TABLE child ALTER CONSTRAINT child_a_fk NOT DEFERRABLE",
    );
    exec(&engine, "UPDATE child SET id = 11 WHERE id = 1");
    error(
        &engine,
        "SET CONSTRAINTS ALL IMMEDIATE",
        "23503",
        "violates foreign key constraint \"child_a_fk\"",
    );
    exec(&engine, "ROLLBACK");
    assert_eq!(
        engine
            .sql("SELECT id FROM child WHERE a_id = 1", &[])
            .unwrap()
            .rows[0]["id"],
        Value::Int(1)
    );
    assert!(bools(&engine, "child_a_fk").0);

    exec(&engine, "ALTER TABLE parent_a ADD COLUMN root_id INTEGER");
    exec(
        &engine,
        "ALTER TABLE parent_a ADD CONSTRAINT parent_root_fk FOREIGN KEY (root_id) REFERENCES root(id) DEFERRABLE INITIALLY IMMEDIATE",
    );
    exec(&engine, "INSERT INTO root VALUES (10)");
    exec(&engine, "UPDATE parent_a SET root_id = 10 WHERE id = 1");
    exec(&engine, "BEGIN");
    exec(&engine, "SET CONSTRAINTS child_a_fk DEFERRED");
    exec(&engine, "DELETE FROM parent_a WHERE id = 1");
    exec(
        &engine,
        "ALTER TABLE child ALTER CONSTRAINT child_a_fk NOT DEFERRABLE",
    );
    error(
        &engine,
        "ALTER TABLE parent_a ALTER CONSTRAINT parent_root_fk NOT DEFERRABLE",
        "55006",
        "pending trigger events",
    );
    exec(&engine, "ROLLBACK");

    exec(&engine, "BEGIN");
    exec(&engine, "SET CONSTRAINTS child_a_fk DEFERRED");
    exec(&engine, "DELETE FROM parent_a WHERE id = 1");
    exec(
        &engine,
        "ALTER TABLE child ALTER CONSTRAINT child_a_fk NOT DEFERRABLE",
    );
    error(
        &engine,
        "COMMIT",
        "23503",
        "violates foreign key constraint \"child_a_fk\"",
    );
    assert_eq!(
        engine
            .sql("SELECT count(*) AS count FROM parent_a WHERE id = 1", &[])
            .unwrap()
            .rows[0]["count"],
        Value::Int(1)
    );
    assert!(bools(&engine, "child_a_fk").0);
}

#[test]
fn deferred_checks_run_before_on_commit_drop_actions() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TEMP TABLE temp_parent (id INTEGER PRIMARY KEY) ON COMMIT PRESERVE ROWS",
    );
    exec(&engine, "BEGIN");
    exec(
        &engine,
        "CREATE TEMP TABLE temp_child (id INTEGER PRIMARY KEY, parent_id INTEGER, CONSTRAINT temp_child_parent_fk FOREIGN KEY (parent_id) REFERENCES temp_parent(id) DEFERRABLE INITIALLY IMMEDIATE) ON COMMIT DROP",
    );
    exec(&engine, "SET CONSTRAINTS temp_child_parent_fk DEFERRED");
    exec(&engine, "INSERT INTO temp_child VALUES (1, 101)");
    error(
        &engine,
        "COMMIT",
        "23503",
        "violates foreign key constraint \"temp_child_parent_fk\"",
    );
    assert!(engine.has_table("temp_parent").unwrap());
    assert!(!engine.has_table("temp_child").unwrap());
}

#[test]
fn parent_events_are_queued_without_referencing_rows() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE parent (id INTEGER PRIMARY KEY)");
    exec(
        &engine,
        "CREATE TABLE child (parent_id INTEGER, CONSTRAINT child_parent_fk FOREIGN KEY (parent_id) REFERENCES parent(id) DEFERRABLE INITIALLY IMMEDIATE)",
    );
    exec(&engine, "INSERT INTO parent VALUES (1)");

    for mutation in [
        "DELETE FROM parent WHERE id = 1",
        "UPDATE parent SET id = 2 WHERE id = 1",
    ] {
        exec(&engine, "BEGIN");
        exec(&engine, "SET CONSTRAINTS child_parent_fk DEFERRED");
        exec(&engine, mutation);
        error(
            &engine,
            "ALTER TABLE parent ADD COLUMN blocked INTEGER",
            "55006",
            "pending trigger events",
        );
        exec(&engine, "ROLLBACK");
    }

    exec(&engine, "BEGIN");
    exec(&engine, "SET CONSTRAINTS child_parent_fk DEFERRED");
    exec(&engine, "DELETE FROM parent WHERE id = 1");
    exec(&engine, "SET CONSTRAINTS child_parent_fk IMMEDIATE");
    exec(&engine, "ALTER TABLE parent ADD COLUMN allowed INTEGER");
    exec(&engine, "COMMIT");
}

#[test]
fn drop_restrict_dependency_precedes_a_pending_parent_event() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE parent (id INTEGER PRIMARY KEY)");
    exec(
        &engine,
        "CREATE TABLE child (parent_id INTEGER, CONSTRAINT child_parent_fk FOREIGN KEY (parent_id) REFERENCES parent(id) DEFERRABLE INITIALLY IMMEDIATE)",
    );
    exec(&engine, "INSERT INTO parent VALUES (1)");
    exec(&engine, "INSERT INTO child VALUES (1)");

    exec(&engine, "BEGIN");
    exec(&engine, "SET CONSTRAINTS child_parent_fk DEFERRED");
    exec(&engine, "DELETE FROM parent WHERE id = 1");
    error(
        &engine,
        "DROP TABLE parent",
        "2BP01",
        "other objects depend on it",
    );
    exec(&engine, "ROLLBACK");

    exec(&engine, "CREATE VIEW child_view AS SELECT * FROM child");
    exec(&engine, "BEGIN");
    exec(&engine, "SET CONSTRAINTS child_parent_fk DEFERRED");
    exec(&engine, "INSERT INTO child VALUES (999)");
    error(
        &engine,
        "DROP TABLE child",
        "2BP01",
        "other objects depend on it",
    );
    exec(&engine, "ROLLBACK");

    exec(&engine, "BEGIN");
    exec(&engine, "SET CONSTRAINTS child_parent_fk DEFERRED");
    exec(&engine, "DELETE FROM parent WHERE id = 1");
    error(
        &engine,
        "DROP TABLE parent CASCADE",
        "55006",
        "pending trigger events",
    );
    exec(&engine, "ROLLBACK");
}

#[path = "pg18_constraint_lifecycle/catalog_lifecycle.rs"]
mod catalog_lifecycle;
