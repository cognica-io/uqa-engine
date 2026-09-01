//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

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
