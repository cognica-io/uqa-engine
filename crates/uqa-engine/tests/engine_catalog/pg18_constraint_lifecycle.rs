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

#[test]
fn simple_query_batch_restarts_after_an_existing_transaction_ends() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE items (id INTEGER PRIMARY KEY)");

    exec(&engine, "BEGIN");
    exec(&engine, "INSERT INTO items VALUES (1)");
    error(
        &engine,
        "COMMIT; INSERT INTO items VALUES (2); INSERT INTO items VALUES (2)",
        "23505",
        "violated",
    );
    assert_eq!(
        engine
            .sql("SELECT count(*) AS count FROM items", &[])
            .unwrap()
            .rows[0]["count"],
        Value::Int(1)
    );

    exec(&engine, "BEGIN");
    exec(&engine, "INSERT INTO items VALUES (3)");
    error(
        &engine,
        "ROLLBACK; INSERT INTO items VALUES (4); INSERT INTO items VALUES (4)",
        "23505",
        "violated",
    );
    assert_eq!(
        engine.sql("SELECT id FROM items", &[]).unwrap().rows[0]["id"],
        Value::Int(1)
    );
}

#[test]
fn pending_events_block_relation_rewrites_but_allow_renames() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE parent (id INTEGER PRIMARY KEY)");
    exec(
        &engine,
        "CREATE TABLE child (id INTEGER PRIMARY KEY, parent_id INTEGER, marker INTEGER, CONSTRAINT child_parent_fk FOREIGN KEY (parent_id) REFERENCES parent(id) DEFERRABLE INITIALLY IMMEDIATE)",
    );

    for statement in [
        "ALTER TABLE child ADD COLUMN added INTEGER",
        "ALTER TABLE child DROP CONSTRAINT child_parent_fk",
        "DROP TABLE child",
        "TRUNCATE child",
    ] {
        exec(&engine, "BEGIN");
        exec(&engine, "SET CONSTRAINTS child_parent_fk DEFERRED");
        exec(&engine, "INSERT INTO child VALUES (1, 101, 0)");
        error(&engine, statement, "55006", "pending trigger events");
        exec(&engine, "ROLLBACK");
    }

    exec(&engine, "BEGIN");
    exec(&engine, "SET CONSTRAINTS child_parent_fk DEFERRED");
    exec(&engine, "INSERT INTO child VALUES (2, 202, 0)");
    exec(
        &engine,
        "ALTER TABLE child RENAME COLUMN marker TO renamed_marker",
    );
    exec(&engine, "ALTER TABLE child RENAME TO renamed_child");
    exec(&engine, "INSERT INTO parent VALUES (202)");
    exec(&engine, "COMMIT");
    assert_eq!(
        engine
            .sql("SELECT renamed_marker FROM renamed_child WHERE id = 2", &[],)
            .unwrap()
            .rows[0]["renamed_marker"],
        Value::Int(0)
    );

    exec(&engine, "BEGIN");
    exec(&engine, "SET CONSTRAINTS child_parent_fk DEFERRED");
    exec(
        &engine,
        "UPDATE renamed_child SET renamed_marker = 1 WHERE id = 2",
    );
    exec(
        &engine,
        "ALTER TABLE renamed_child ADD COLUMN update_safe INTEGER",
    );
    exec(&engine, "COMMIT");

    exec(&engine, "BEGIN");
    exec(&engine, "SET CONSTRAINTS child_parent_fk DEFERRED");
    exec(&engine, "DELETE FROM renamed_child WHERE id = 2");
    exec(
        &engine,
        "ALTER TABLE renamed_child ADD COLUMN delete_safe INTEGER",
    );
    exec(&engine, "COMMIT");
    assert!(engine
        .sql("SELECT id FROM renamed_child", &[])
        .unwrap()
        .rows
        .is_empty());
}

#[test]
fn partition_deferred_events_keep_their_physical_identity_during_unrelated_ddl() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE parent (id INTEGER PRIMARY KEY)");
    exec(
        &engine,
        "CREATE TABLE partitioned_child (id INTEGER PRIMARY KEY, parent_id INTEGER, CONSTRAINT partitioned_child_parent_fk FOREIGN KEY (parent_id) REFERENCES parent(id) DEFERRABLE INITIALLY IMMEDIATE) PARTITION BY RANGE (id)",
    );
    exec(
        &engine,
        "CREATE TABLE partitioned_child_low PARTITION OF partitioned_child FOR VALUES FROM (0) TO (10)",
    );
    exec(&engine, "CREATE TABLE unrelated (id INTEGER)");

    exec(&engine, "BEGIN");
    exec(
        &engine,
        "SET CONSTRAINTS partitioned_child_parent_fk DEFERRED",
    );
    exec(&engine, "INSERT INTO partitioned_child VALUES (1, 101)");
    exec(
        &engine,
        "ALTER TABLE unrelated ADD CONSTRAINT unrelated_positive CHECK (id > 0)",
    );
    error(
        &engine,
        "COMMIT",
        "23503",
        "violates foreign key constraint \"partitioned_child_parent_fk\"",
    );
    assert!(engine
        .sql("SELECT id FROM partitioned_child", &[])
        .unwrap()
        .rows
        .is_empty());
}

#[test]
fn constraint_modes_do_not_transfer_to_cross_session_replacements() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("constraint-identity.db");
    let session_a = Engine::open(&database).unwrap();
    exec(&session_a, "CREATE TABLE parent_a (id INTEGER PRIMARY KEY)");
    exec(&session_a, "CREATE TABLE parent_b (id INTEGER PRIMARY KEY)");
    exec(
        &session_a,
        "CREATE TABLE child (id INTEGER PRIMARY KEY, parent_id INTEGER, CONSTRAINT child_parent_fk FOREIGN KEY (parent_id) REFERENCES parent_a(id) DEFERRABLE INITIALLY IMMEDIATE)",
    );
    let session_b = Engine::open(&database).unwrap();

    exec(&session_a, "BEGIN");
    exec(&session_a, "SET CONSTRAINTS child_parent_fk DEFERRED");
    exec(
        &session_b,
        "ALTER TABLE child DROP CONSTRAINT child_parent_fk",
    );
    exec(
        &session_b,
        "ALTER TABLE child ADD CONSTRAINT child_parent_fk FOREIGN KEY (parent_id) REFERENCES parent_b(id) DEFERRABLE INITIALLY IMMEDIATE",
    );
    error(
        &session_a,
        "INSERT INTO child VALUES (1, 101)",
        "23503",
        "violates foreign key constraint \"child_parent_fk\"",
    );
    exec(&session_a, "ROLLBACK");
    assert!(session_b
        .sql("SELECT id FROM child", &[])
        .unwrap()
        .rows
        .is_empty());
}

#[test]
fn constraint_modes_follow_cross_session_table_renames_by_object_identity() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("constraint-rename-identity.db");
    let session_a = Engine::open(&database).unwrap();
    exec(&session_a, "CREATE TABLE parent (id INTEGER PRIMARY KEY)");
    exec(
        &session_a,
        "CREATE TABLE child (id INTEGER PRIMARY KEY, parent_id INTEGER, CONSTRAINT child_parent_fk FOREIGN KEY (parent_id) REFERENCES parent(id) DEFERRABLE INITIALLY IMMEDIATE)",
    );
    let session_b = Engine::open(&database).unwrap();

    exec(&session_a, "BEGIN");
    exec(&session_a, "SET CONSTRAINTS child_parent_fk DEFERRED");
    exec(&session_b, "ALTER TABLE child RENAME TO renamed_child");
    exec(&session_a, "INSERT INTO renamed_child VALUES (1, 101)");
    exec(&session_a, "INSERT INTO parent VALUES (101)");
    exec(&session_a, "COMMIT");
    assert_eq!(
        session_b
            .sql("SELECT parent_id FROM renamed_child WHERE id = 1", &[],)
            .unwrap()
            .rows[0]["parent_id"],
        Value::Int(101)
    );
}

#[test]
fn set_constraints_uses_batch_callback_and_temporary_namespace_transaction_contexts() {
    let engine = Arc::new(Engine::new());
    exec(&engine, "CREATE TABLE parent (id INTEGER PRIMARY KEY)");
    exec(
        &engine,
        "CREATE TABLE child (id INTEGER PRIMARY KEY, parent_id INTEGER, CONSTRAINT child_parent_fk FOREIGN KEY (parent_id) REFERENCES parent(id) DEFERRABLE INITIALLY IMMEDIATE)",
    );
    exec(
        &engine,
        "SET CONSTRAINTS child_parent_fk DEFERRED; INSERT INTO child VALUES (1, 101); INSERT INTO parent VALUES (101); COMMIT",
    );
    assert!(engine.take_sql_notices().iter().any(|(level, message)| {
        level == "WARNING" && message == "there is no transaction in progress"
    }));
    error(
        &engine,
        "INSERT INTO parent VALUES (303); INSERT INTO parent VALUES (303); COMMIT",
        "23505",
        "violated",
    );
    assert!(engine
        .sql("SELECT id FROM parent WHERE id = 303", &[])
        .unwrap()
        .rows
        .is_empty());

    exec(
        &engine,
        "CREATE TABLE batch_control (id INTEGER PRIMARY KEY)",
    );
    exec(&engine, "INSERT INTO batch_control VALUES (1); ROLLBACK");
    assert!(engine
        .sql("SELECT id FROM batch_control", &[])
        .unwrap()
        .rows
        .is_empty());
    error(
        &engine,
        "INSERT INTO batch_control VALUES (2); SAVEPOINT batch_savepoint",
        "25P01",
        "SAVEPOINT can only be used in transaction blocks",
    );
    assert!(engine
        .sql("SELECT id FROM batch_control", &[])
        .unwrap()
        .rows
        .is_empty());
    exec(
        &engine,
        "INSERT INTO batch_control VALUES (3); BEGIN; INSERT INTO batch_control VALUES (4); ROLLBACK",
    );
    assert!(engine
        .sql("SELECT id FROM batch_control", &[])
        .unwrap()
        .rows
        .is_empty());

    let callback_engine: Weak<Engine> = Arc::downgrade(&engine);
    engine
        .register_scalar_function(
            "insert_child_parent_pair",
            move |_args: &[Value]| -> Result<Value, SQLError> {
                let engine = callback_engine
                    .upgrade()
                    .ok_or_else(|| SQLError::Internal("callback engine was dropped".into()))?;
                engine.sql("SET CONSTRAINTS child_parent_fk DEFERRED", &[])?;
                engine.sql("INSERT INTO child VALUES (2, 202)", &[])?;
                engine.sql("INSERT INTO parent VALUES (202)", &[])?;
                Ok(Value::Int(1))
            },
        )
        .unwrap();
    engine.take_sql_notices();
    exec(&engine, "SELECT insert_child_parent_pair()");
    assert!(engine.take_sql_notices().is_empty());

    engine.take_sql_notices();
    error(
        &engine,
        "SET CONSTRAINTS missing_constraint DEFERRED",
        "42704",
        "does not exist",
    );
    assert!(engine.take_sql_notices().iter().any(|(level, message)| {
        level == "WARNING" && message == "SET CONSTRAINTS can only be used in transaction blocks"
    }));
    exec(&engine, "CREATE TEMP TABLE temp_lifetime (id INTEGER)");
    exec(&engine, "DROP TABLE temp_lifetime");
    exec(&engine, "BEGIN");
    error(
        &engine,
        "SET CONSTRAINTS pg_temp.missing_constraint DEFERRED",
        "42704",
        "does not exist",
    );
    exec(&engine, "ROLLBACK");
}

#[test]
fn temporary_namespace_allocation_rolls_back_with_its_first_temporary_object() {
    let engine = Engine::new();
    exec(&engine, "BEGIN");
    exec(&engine, "CREATE TEMP TABLE rolled_back_temp (id INTEGER)");
    exec(&engine, "ROLLBACK");
    exec(&engine, "BEGIN");
    error(
        &engine,
        "SET CONSTRAINTS pg_temp.missing_constraint DEFERRED",
        "3F000",
        "schema \"pg_temp\" does not exist",
    );
    exec(&engine, "ROLLBACK");
}

#[test]
fn set_constraints_immediate_checks_only_the_selected_foreign_keys() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE parent_a (id INTEGER PRIMARY KEY)");
    exec(&engine, "CREATE TABLE parent_b (id INTEGER PRIMARY KEY)");
    exec(
        &engine,
        "CREATE TABLE child (id INTEGER PRIMARY KEY, a_id INTEGER, b_id INTEGER, CONSTRAINT child_a_fk FOREIGN KEY (a_id) REFERENCES parent_a(id) DEFERRABLE INITIALLY DEFERRED, CONSTRAINT child_b_fk FOREIGN KEY (b_id) REFERENCES parent_b(id) DEFERRABLE INITIALLY DEFERRED)",
    );

    exec(&engine, "BEGIN");
    exec(&engine, "INSERT INTO child VALUES (1, 10, 20)");
    exec(&engine, "INSERT INTO parent_a VALUES (10)");
    exec(&engine, "SET CONSTRAINTS child_a_fk IMMEDIATE");
    exec(&engine, "INSERT INTO parent_b VALUES (20)");
    exec(&engine, "SET CONSTRAINTS child_b_fk IMMEDIATE");
    exec(&engine, "COMMIT");

    exec(&engine, "BEGIN");
    exec(&engine, "INSERT INTO child VALUES (2, 11, 21)");
    exec(&engine, "INSERT INTO parent_b VALUES (21)");
    exec(&engine, "SAVEPOINT before_selected_check");
    error(
        &engine,
        "SET CONSTRAINTS child_a_fk IMMEDIATE",
        "23503",
        "violates foreign key constraint \"child_a_fk\"",
    );
    exec(&engine, "ROLLBACK TO SAVEPOINT before_selected_check");
    exec(&engine, "INSERT INTO parent_a VALUES (11)");
    exec(&engine, "SET CONSTRAINTS child_a_fk, child_b_fk IMMEDIATE");
    exec(&engine, "COMMIT");
}

#[test]
fn set_constraints_resolves_names_like_postgresql_and_all_ignores_immediate_constraints() {
    let engine = Engine::new();
    exec(&engine, "CREATE SCHEMA first");
    exec(&engine, "CREATE SCHEMA second");
    exec(
        &engine,
        "CREATE TABLE first.checked (id INTEGER CONSTRAINT shared CHECK (id > 0))",
    );
    exec(
        &engine,
        "CREATE TABLE second.parent (id INTEGER PRIMARY KEY)",
    );
    exec(
        &engine,
        "CREATE TABLE second.child_one (id INTEGER PRIMARY KEY, parent_id INTEGER, CONSTRAINT shared FOREIGN KEY (parent_id) REFERENCES second.parent(id) DEFERRABLE INITIALLY IMMEDIATE)",
    );
    exec(
        &engine,
        "CREATE TABLE second.child_two (id INTEGER PRIMARY KEY, parent_id INTEGER, CONSTRAINT shared FOREIGN KEY (parent_id) REFERENCES second.parent(id) DEFERRABLE INITIALLY IMMEDIATE)",
    );

    exec(&engine, "SET search_path TO first, second, public");
    exec(&engine, "BEGIN");
    error(
        &engine,
        "SET CONSTRAINTS shared DEFERRED",
        "42809",
        "constraint \"shared\" is not deferrable",
    );
    exec(&engine, "ROLLBACK");
    exec(&engine, "BEGIN");
    exec(&engine, "SET CONSTRAINTS shared IMMEDIATE");
    exec(&engine, "COMMIT");

    exec(&engine, "SET search_path TO second, first, public");
    exec(&engine, "BEGIN");
    exec(&engine, "SET CONSTRAINTS shared DEFERRED");
    exec(&engine, "INSERT INTO second.child_one VALUES (1, 11)");
    exec(&engine, "INSERT INTO second.child_two VALUES (2, 22)");
    exec(&engine, "INSERT INTO second.parent VALUES (11)");
    exec(&engine, "SAVEPOINT before_all_matches_check");
    error(
        &engine,
        "SET CONSTRAINTS shared IMMEDIATE",
        "23503",
        "violates foreign key constraint \"shared\"",
    );
    exec(&engine, "ROLLBACK TO SAVEPOINT before_all_matches_check");
    exec(&engine, "INSERT INTO second.parent VALUES (22)");
    exec(&engine, "SET CONSTRAINTS uqa.second.shared IMMEDIATE");
    exec(&engine, "COMMIT");

    exec(&engine, "BEGIN");
    exec(&engine, "SET CONSTRAINTS ALL DEFERRED");
    exec(&engine, "INSERT INTO second.child_one VALUES (3, 33)");
    exec(&engine, "INSERT INTO second.parent VALUES (33)");
    exec(&engine, "COMMIT");

    exec(&engine, "BEGIN");
    error(
        &engine,
        "SET CONSTRAINTS missing_constraint DEFERRED",
        "42704",
        "does not exist",
    );
    exec(&engine, "ROLLBACK");
    exec(&engine, "BEGIN");
    error(
        &engine,
        "SET CONSTRAINTS other.second.shared DEFERRED",
        "0A000",
        "cross-database references are not implemented",
    );
    exec(&engine, "ROLLBACK");
    exec(&engine, "BEGIN");
    error(
        &engine,
        "SET CONSTRAINTS absent_schema.shared DEFERRED",
        "3F000",
        "schema \"absent_schema\" does not exist",
    );
    exec(&engine, "ROLLBACK");

    exec(&engine, "SET CONSTRAINTS ALL IMMEDIATE");
    assert!(engine.take_sql_notices().iter().any(|(level, message)| {
        level == "WARNING" && message == "SET CONSTRAINTS can only be used in transaction blocks"
    }));
}

#[test]
fn set_constraints_honors_explicit_pg_temp_search_path_position() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE public.path_parent (id INTEGER PRIMARY KEY)",
    );
    exec(
        &engine,
        "CREATE TABLE public.path_child (id INTEGER PRIMARY KEY, parent_id INTEGER, CONSTRAINT path_shared FOREIGN KEY (parent_id) REFERENCES public.path_parent(id) DEFERRABLE INITIALLY IMMEDIATE)",
    );
    exec(
        &engine,
        "CREATE TEMP TABLE temp_parent (id INTEGER PRIMARY KEY)",
    );
    exec(
        &engine,
        "CREATE TEMP TABLE temp_child (id INTEGER PRIMARY KEY, parent_id INTEGER, CONSTRAINT path_shared FOREIGN KEY (parent_id) REFERENCES temp_parent(id) DEFERRABLE INITIALLY IMMEDIATE)",
    );
    exec(&engine, "SET search_path TO public, pg_temp");
    exec(&engine, "BEGIN");
    exec(&engine, "SET CONSTRAINTS path_shared DEFERRED");
    exec(&engine, "INSERT INTO public.path_child VALUES (1, 1001)");
    exec(&engine, "SAVEPOINT temp_constraint_is_immediate");
    error(
        &engine,
        "INSERT INTO pg_temp.temp_child VALUES (1, 2001)",
        "23503",
        "violates foreign key constraint \"path_shared\"",
    );
    exec(
        &engine,
        "ROLLBACK TO SAVEPOINT temp_constraint_is_immediate",
    );
    exec(&engine, "INSERT INTO public.path_parent VALUES (1001)");
    exec(&engine, "COMMIT");
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
