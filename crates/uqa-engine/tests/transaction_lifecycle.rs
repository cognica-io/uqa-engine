//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Engine-level transaction lifecycle convenience methods for begin, commit,
//! rollback, and savepoint operations.

use uqa_engine::Engine;
use uqa_storage::document_store::Document;

fn shown(engine: &Engine, name: &str) -> String {
    let result = engine.sql(&format!("SHOW {name}"), &[]).unwrap();
    let uqa_core::Value::Str(value) = &result.rows[0][name] else {
        panic!("SHOW {name} did not return text");
    };
    value.clone()
}

fn integer_column(result: &uqa_sql::SQLResult, name: &str) -> Vec<i64> {
    result
        .rows
        .iter()
        .map(|row| match row.get(name) {
            Some(uqa_core::Value::Int(value)) => *value,
            other => panic!("expected integer column {name}, got {other:?}"),
        })
        .collect()
}

#[test]
fn begin_commit_round_trip() {
    let eng = Engine::new();
    assert_eq!(eng.transaction_depth(), 0);
    eng.begin().unwrap();
    assert_eq!(eng.transaction_depth(), 1);
    eng.commit().unwrap();
    assert_eq!(eng.transaction_depth(), 0);
}

#[test]
fn nested_begin_commit_pops_one_frame_at_a_time() {
    let eng = Engine::new();
    eng.begin().unwrap();
    eng.begin().unwrap();
    assert_eq!(eng.transaction_depth(), 2);
    eng.commit().unwrap();
    assert_eq!(eng.transaction_depth(), 1);
    eng.rollback().unwrap();
    assert_eq!(eng.transaction_depth(), 0);
}

#[test]
fn user_savepoint_names_cannot_alias_nested_transaction_checkpoints() {
    let directory = tempfile::tempdir().unwrap();
    let eng = Engine::open(&directory.path().join("savepoint-identity.db")).unwrap();
    eng.sql("CREATE TABLE savepoint_rows (id INTEGER)", &[])
        .unwrap();
    eng.begin().unwrap();
    eng.sql("INSERT INTO savepoint_rows VALUES (1)", &[])
        .unwrap();
    eng.begin().unwrap();
    eng.sql("INSERT INTO savepoint_rows VALUES (2)", &[])
        .unwrap();
    eng.savepoint("__uqa_nested_tx_1").unwrap();
    eng.sql("INSERT INTO savepoint_rows VALUES (3)", &[])
        .unwrap();

    eng.rollback().unwrap();
    eng.commit().unwrap();

    let rows = eng
        .sql("SELECT id FROM savepoint_rows ORDER BY id", &[])
        .unwrap();
    assert_eq!(rows.rows.len(), 1);
    assert_eq!(rows.rows[0]["id"], uqa_core::Value::Int(1));
}

#[test]
fn savepoint_release_round_trip() {
    let eng = Engine::new();
    eng.begin().unwrap();
    eng.savepoint("sp1").unwrap();
    eng.release_savepoint("sp1").unwrap();
    eng.commit().unwrap();
    assert_eq!(eng.transaction_depth(), 0);
}

#[test]
fn rollback_to_savepoint_keeps_frame_open() {
    let eng = Engine::new();
    eng.begin().unwrap();
    eng.savepoint("sp1").unwrap();
    eng.rollback_to_savepoint("sp1").unwrap();
    assert_eq!(eng.transaction_depth(), 1);
    eng.rollback().unwrap();
    assert_eq!(eng.transaction_depth(), 0);
}

#[test]
fn pg18_transaction_characteristics_follow_query_and_savepoint_scope() {
    let eng = Engine::new();
    assert_eq!(
        shown(&eng, "default_transaction_isolation"),
        "read committed"
    );
    assert_eq!(shown(&eng, "transaction_isolation"), "read committed");
    assert_eq!(shown(&eng, "transaction_read_only"), "off");
    assert_eq!(shown(&eng, "transaction_deferrable"), "off");

    eng.sql(
        "START TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ WRITE, DEFERRABLE",
        &[],
    )
    .unwrap();
    assert_eq!(shown(&eng, "transaction_isolation"), "repeatable read");
    assert_eq!(shown(&eng, "transaction_read_only"), "off");
    assert_eq!(shown(&eng, "transaction_deferrable"), "on");
    eng.sql("SELECT 1", &[]).unwrap();
    eng.sql("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ", &[])
        .unwrap();
    let error = eng
        .sql("SET TRANSACTION ISOLATION LEVEL SERIALIZABLE", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("25001"));
    eng.sql("ROLLBACK", &[]).unwrap();

    eng.sql("BEGIN DEFERRABLE", &[]).unwrap();
    eng.sql("SAVEPOINT deferrable_scope", &[]).unwrap();
    let error = eng.sql("SET TRANSACTION DEFERRABLE", &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("25001"), "{error}");
    assert!(error.to_string().contains("within a subtransaction"));
    eng.sql("ROLLBACK TO SAVEPOINT deferrable_scope", &[])
        .unwrap();
    eng.sql("ROLLBACK", &[]).unwrap();

    eng.sql("BEGIN", &[]).unwrap();
    eng.sql("SAVEPOINT isolation_scope", &[]).unwrap();
    eng.sql("SELECT 1", &[]).unwrap();
    let error = eng
        .sql("SET TRANSACTION ISOLATION LEVEL SERIALIZABLE", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("25001"), "{error}");
    assert!(error.to_string().contains("before any query"));
    eng.sql("ROLLBACK", &[]).unwrap();

    eng.sql("BEGIN READ WRITE", &[]).unwrap();
    eng.sql("SAVEPOINT mode_scope", &[]).unwrap();
    eng.sql("SET TRANSACTION READ ONLY", &[]).unwrap();
    assert_eq!(shown(&eng, "transaction_read_only"), "on");
    eng.sql("RELEASE SAVEPOINT mode_scope", &[]).unwrap();
    assert_eq!(shown(&eng, "transaction_read_only"), "off");
    eng.sql("ROLLBACK", &[]).unwrap();

    for rollback_to_savepoint in [false, true] {
        eng.sql("BEGIN", &[]).unwrap();
        eng.sql("SAVEPOINT snapshot_scope", &[]).unwrap();
        eng.sql("SELECT 1", &[]).unwrap();
        if rollback_to_savepoint {
            eng.sql("ROLLBACK TO SAVEPOINT snapshot_scope", &[])
                .unwrap();
        }
        eng.sql("RELEASE SAVEPOINT snapshot_scope", &[]).unwrap();
        let error = eng
            .sql("SET TRANSACTION ISOLATION LEVEL SERIALIZABLE", &[])
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("25001"), "{error}");
        eng.sql("ROLLBACK", &[]).unwrap();
    }

    eng.sql("CREATE TABLE transaction_mode_rows (id INTEGER)", &[])
        .unwrap();
    eng.sql("BEGIN READ ONLY", &[]).unwrap();
    eng.sql("SET TRANSACTION READ WRITE", &[]).unwrap();
    eng.sql("INSERT INTO transaction_mode_rows VALUES (1)", &[])
        .unwrap();
    eng.sql("COMMIT", &[]).unwrap();
    assert_eq!(
        integer_column(
            &eng.sql("SELECT id FROM transaction_mode_rows", &[])
                .unwrap(),
            "id"
        ),
        [1]
    );
}

fn exercise_pg18_read_only_dml_edges(eng: &Engine) {
    for sql in [
        "DROP TABLE permanent_rows",
        "INSERT INTO permanent_rows VALUES (1)",
        "CREATE TABLE copied_rows AS SELECT * FROM permanent_rows",
        "CREATE TEMP TABLE another_temporary_row (id INTEGER)",
        "ALTER TABLE temporary_rows ADD COLUMN another INTEGER",
        "TRUNCATE temporary_rows",
        "DROP TABLE temporary_rows",
        "SELECT nextval('permanent_sequence')",
        "SELECT setval('permanent_sequence', 42)",
        "SELECT mutate_read_only()",
        "SELECT * FROM permanent_rows FOR UPDATE",
    ] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("25006"), "{sql}: {error}");
    }
    for sql in [
        "INSERT INTO missing_read_only_relation VALUES (1)",
        "UPDATE missing_read_only_relation SET id = 1",
        "DELETE FROM missing_read_only_relation",
        "SELECT * FROM missing_read_only_relation FOR UPDATE",
    ] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42P01"), "{sql}: {error}");
    }
    let error = eng
        .sql(
            "INSERT INTO temporary_rows
             SELECT x FROM cypher('readonly_escape', $$
                CREATE (n) RETURN 2
             $$) AS (x integer)",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("25006"), "{error}");
    let dynamic_query = [uqa_sql::SQLParam::scalar(uqa_core::Value::Str(
        "CREATE (n) RETURN 3".into(),
    ))];
    let error = eng
        .sql(
            "INSERT INTO temporary_rows
             SELECT x FROM cypher('readonly_escape', $1) AS (x integer)",
            &dynamic_query,
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("25006"), "{error}");
    assert!(eng
        .sql(
            "SELECT x FROM cypher('readonly_escape', $$
                MATCH (n) RETURN 1
             $$) AS (x integer)",
            &[],
        )
        .unwrap()
        .rows
        .is_empty());
    eng.sql("DELETE FROM temporary_rows", &[]).unwrap();
    eng.sql(
        "UPDATE temporary_rows SET id = 0 FROM permanent_rows WHERE temporary_rows.id = permanent_rows.id",
        &[],
    )
    .unwrap();
    eng.sql(
        "PREPARE permanent_update AS UPDATE permanent_rows SET id = 0",
        &[],
    )
    .unwrap();
    let error = eng.sql("EXECUTE permanent_update", &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("25006"));
    assert!(eng
        .sql("SELECT * FROM permanent_rows WHERE id IN (99, 314)", &[])
        .unwrap()
        .rows
        .is_empty());
}

#[test]
fn pg18_read_only_transactions_allow_only_temporary_relation_dml() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE permanent_rows (id INTEGER)", &[])
        .unwrap();
    eng.sql("CREATE TEMP TABLE temporary_rows (id INTEGER)", &[])
        .unwrap();
    eng.sql("CREATE SEQUENCE permanent_sequence", &[]).unwrap();
    eng.sql("CREATE TEMP SEQUENCE temporary_sequence", &[])
        .unwrap();
    eng.sql(
        "CREATE FUNCTION pure_read_only() RETURNS INTEGER LANGUAGE SQL AS 'SELECT 1'",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE FUNCTION mutate_read_only() RETURNS INTEGER LANGUAGE SQL AS 'INSERT INTO permanent_rows VALUES (99) RETURNING id' VOLATILE",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE FUNCTION exception_write_read_only() RETURNS INTEGER LANGUAGE plpgsql AS 'BEGIN BEGIN INSERT INTO permanent_rows VALUES (314); EXCEPTION WHEN OTHERS THEN NULL; END; RETURN 1; END' VOLATILE",
        &[],
    )
    .unwrap();
    eng.sql("CREATE TABLE readonly_search (id INTEGER, body TEXT)", &[])
        .unwrap();
    eng.sql(
        "CREATE INDEX readonly_search_body_gin ON readonly_search USING gin (body)",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO readonly_search VALUES (1, 'hello world')", &[])
        .unwrap();
    eng.sql("SELECT create_graph('readonly_escape')", &[])
        .unwrap();
    eng.sql("INSERT INTO temporary_rows VALUES (1)", &[])
        .unwrap();
    eng.sql("SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY", &[])
        .unwrap();
    eng.sql("SELECT random()", &[]).unwrap();
    eng.sql("SELECT setseed(0.5)", &[]).unwrap();
    assert_eq!(
        integer_column(
            &eng.sql("SELECT pure_read_only() AS value", &[]).unwrap(),
            "value"
        ),
        [1]
    );
    eng.sql("SELECT nextval('temporary_sequence')", &[])
        .unwrap();
    eng.sql("SELECT setval('temporary_sequence', 42)", &[])
        .unwrap();
    eng.sql("ANALYZE permanent_rows", &[]).unwrap();
    assert_eq!(
        integer_column(
            &eng.sql(
                "SELECT id FROM readonly_search WHERE fts_match(body, 'hello')",
                &[],
            )
            .unwrap(),
            "id"
        ),
        [1]
    );
    assert_eq!(
        integer_column(
            &eng.sql("SELECT exception_write_read_only() AS value", &[])
                .unwrap(),
            "value"
        ),
        [1]
    );
    eng.sql("SELECT * FROM temporary_rows FOR UPDATE", &[])
        .unwrap();
    exercise_pg18_read_only_dml_edges(&eng);

    eng.sql("START TRANSACTION READ ONLY", &[]).unwrap();
    let locking_cursor = eng
        .sql(
            "DECLARE read_only_locking_cursor CURSOR FOR SELECT * FROM permanent_rows FOR UPDATE",
            &[],
        )
        .unwrap_err();
    assert_eq!(locking_cursor.sqlstate(), Some("25006"), "{locking_cursor}");
    eng.sql("ROLLBACK", &[]).unwrap();

    eng.sql("START TRANSACTION READ WRITE", &[]).unwrap();
    eng.sql("DROP TABLE permanent_rows", &[]).unwrap();
    eng.sql("COMMIT", &[]).unwrap();
    eng.sql("RESET default_transaction_read_only", &[]).unwrap();
    assert_eq!(shown(&eng, "transaction_read_only"), "off");
}

#[test]
fn pg18_commit_and_rollback_chain_keep_current_characteristics() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE chained_rows (id INTEGER)", &[])
        .unwrap();
    eng.sql("SET default_transaction_read_only = on", &[])
        .unwrap();
    eng.sql(
        "START TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ WRITE, DEFERRABLE",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO chained_rows VALUES (1)", &[]).unwrap();
    eng.sql("COMMIT AND CHAIN", &[]).unwrap();
    assert_eq!(shown(&eng, "transaction_isolation"), "repeatable read");
    assert_eq!(shown(&eng, "transaction_read_only"), "off");
    assert_eq!(shown(&eng, "transaction_deferrable"), "on");
    eng.sql("INSERT INTO chained_rows VALUES (2)", &[]).unwrap();
    eng.sql("ROLLBACK AND CHAIN", &[]).unwrap();
    assert_eq!(shown(&eng, "transaction_isolation"), "repeatable read");
    assert_eq!(shown(&eng, "transaction_read_only"), "off");
    eng.sql("ROLLBACK", &[]).unwrap();

    assert_eq!(shown(&eng, "transaction_read_only"), "on");
    eng.sql("RESET default_transaction_read_only", &[]).unwrap();
    for sql in ["COMMIT AND CHAIN", "ROLLBACK AND CHAIN"] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("25P01"));
    }
}

#[test]
fn pg18_transaction_reset_snapshot_and_savepoint_errors_use_expected_sqlstates() {
    let eng = Engine::new();
    for clause in [
        "SET transaction_read_only = on",
        "SET transaction_read_only FROM CURRENT",
    ] {
        let error = eng
            .sql(
                &format!(
                    "CREATE FUNCTION invalid_transaction_config() RETURNS INTEGER LANGUAGE SQL AS 'SELECT 1' {clause}"
                ),
                &[],
            )
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("0A000"), "{clause}: {error}");
    }
    eng.sql("BEGIN ISOLATION LEVEL SERIALIZABLE", &[]).unwrap();
    let error = eng.sql("RESET transaction_isolation", &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("0A000"));
    eng.sql("ROLLBACK", &[]).unwrap();

    eng.sql("BEGIN", &[]).unwrap();
    let error = eng
        .sql("SET TRANSACTION SNAPSHOT 'FFF-FFF-F'", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("0A000"));
    assert_eq!(
        error.to_string(),
        "a snapshot-importing transaction must have isolation level SERIALIZABLE or REPEATABLE READ"
    );
    eng.sql("ROLLBACK", &[]).unwrap();

    eng.sql("BEGIN ISOLATION LEVEL REPEATABLE READ", &[])
        .unwrap();
    let error = eng
        .sql("SET TRANSACTION SNAPSHOT 'Incorrect Identifier'", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("22023"));
    eng.sql("ROLLBACK", &[]).unwrap();
    eng.sql("BEGIN ISOLATION LEVEL REPEATABLE READ", &[])
        .unwrap();
    let error = eng
        .sql("SET TRANSACTION SNAPSHOT 'FFF-FFF-F'", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42704"));
    eng.sql("ROLLBACK", &[]).unwrap();

    let error = eng.savepoint("outside").unwrap_err();
    assert_eq!(error.sqlstate(), Some("25P01"));
    eng.sql("BEGIN", &[]).unwrap();
    let error = eng.release_savepoint("missing").unwrap_err();
    assert_eq!(error.sqlstate(), Some("3B001"));
    eng.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn pg18_sql_cursor_requires_a_block_and_uses_postgresql_sqlstates() {
    let engine = Engine::new();
    let outside = engine
        .sql("DECLARE c CURSOR FOR SELECT 1 AS value", &[])
        .unwrap_err();
    assert_eq!(outside.sqlstate(), Some("25P01"), "{outside}");
    assert_eq!(
        outside.to_string(),
        "DECLARE CURSOR can only be used in transaction blocks"
    );

    let missing = engine.sql("FETCH FROM missing_cursor", &[]).unwrap_err();
    assert_eq!(missing.sqlstate(), Some("34000"), "{missing}");
    assert_eq!(
        missing.to_string(),
        "cursor \"missing_cursor\" does not exist"
    );
    engine.sql("CLOSE ALL", &[]).unwrap();

    let implicit_batch = engine
        .sql(
            "DECLARE implicit_cursor CURSOR FOR SELECT 1 AS value; FETCH FROM implicit_cursor",
            &[],
        )
        .unwrap();
    assert_eq!(integer_column(&implicit_batch, "value"), [1]);
    assert_eq!(engine.transaction_depth(), 0);
    let closed = engine.sql("FETCH FROM implicit_cursor", &[]).unwrap_err();
    assert_eq!(closed.sqlstate(), Some("34000"), "{closed}");
}

#[test]
fn pg18_sql_cursor_fetch_move_and_savepoint_lifecycle_match_postgresql() {
    let engine = Engine::new();
    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql(
            "DECLARE c SCROLL CURSOR FOR SELECT x FROM (VALUES (1), (2), (3)) AS rows(x) ORDER BY x",
            &[],
        )
        .unwrap();
    let first = engine.sql("FETCH 2 FROM c", &[]).unwrap();
    assert_eq!(integer_column(&first, "x"), [1, 2]);
    engine.sql("SAVEPOINT cursor_position", &[]).unwrap();
    let moved = engine.sql("MOVE BACKWARD 1 FROM c", &[]).unwrap();
    assert_eq!(moved.affected_rows, 1);
    let second = engine.sql("FETCH FROM c", &[]).unwrap();
    assert_eq!(integer_column(&second, "x"), [2]);
    engine
        .sql("ROLLBACK TO SAVEPOINT cursor_position", &[])
        .unwrap();
    let third = engine.sql("FETCH FROM c", &[]).unwrap();
    assert_eq!(integer_column(&third, "x"), [3]);
    let first_again = engine.sql("FETCH ABSOLUTE 1 FROM c", &[]).unwrap();
    assert_eq!(integer_column(&first_again, "x"), [1]);
    let last = engine.sql("FETCH RELATIVE 2 FROM c", &[]).unwrap();
    assert_eq!(integer_column(&last, "x"), [3]);

    let duplicate = engine
        .sql("DECLARE c CURSOR FOR SELECT 4 AS x", &[])
        .unwrap_err();
    assert_eq!(duplicate.sqlstate(), Some("42P03"), "{duplicate}");
    assert_eq!(duplicate.to_string(), "cursor \"c\" already exists");
    engine.sql("ROLLBACK", &[]).unwrap();

    engine.sql("BEGIN", &[]).unwrap();
    engine.sql("SAVEPOINT before_cursor", &[]).unwrap();
    engine
        .sql("DECLARE rolled_back CURSOR FOR SELECT 1 AS value", &[])
        .unwrap();
    engine
        .sql("ROLLBACK TO SAVEPOINT before_cursor", &[])
        .unwrap();
    let removed = engine.sql("FETCH FROM rolled_back", &[]).unwrap_err();
    assert_eq!(removed.sqlstate(), Some("34000"), "{removed}");
    engine.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn pg18_sql_cursor_with_hold_rules_match_postgresql() {
    let engine = Engine::new();
    engine
        .sql(
            "DECLARE autocommit_held CURSOR WITH HOLD FOR SELECT x FROM (VALUES (7), (8)) AS rows(x) ORDER BY x",
            &[],
        )
        .unwrap();
    assert_eq!(
        integer_column(&engine.sql("FETCH FROM autocommit_held", &[]).unwrap(), "x"),
        [7]
    );
    engine.sql("CLOSE autocommit_held", &[]).unwrap();
    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql(
            "DECLARE held CURSOR WITH HOLD FOR SELECT x FROM (VALUES (1), (2), (3)) AS rows(x) ORDER BY x",
            &[],
        )
        .unwrap();
    assert_eq!(
        integer_column(&engine.sql("FETCH FROM held", &[]).unwrap(), "x"),
        [1]
    );
    engine.sql("COMMIT", &[]).unwrap();
    assert_eq!(
        integer_column(&engine.sql("FETCH FROM held", &[]).unwrap(), "x"),
        [2]
    );
    engine.sql("CLOSE held", &[]).unwrap();
}

#[test]
fn pg18_sql_cursor_zero_movement_rules_match_postgresql() {
    let engine = Engine::new();
    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql(
            "DECLARE zero_position NO SCROLL CURSOR FOR SELECT x FROM (VALUES (1), (2)) AS rows(x) ORDER BY x",
            &[],
        )
        .unwrap();
    assert!(engine
        .sql("FETCH FORWARD 0 FROM zero_position", &[])
        .unwrap()
        .rows
        .is_empty());
    assert!(engine
        .sql("FETCH ABSOLUTE 0 FROM zero_position", &[])
        .unwrap()
        .rows
        .is_empty());
    assert_eq!(
        engine
            .sql("MOVE BACKWARD 0 FROM zero_position", &[])
            .unwrap()
            .affected_rows,
        0
    );
    engine.sql("FETCH 1 FROM zero_position", &[]).unwrap();
    assert_eq!(
        engine
            .sql("MOVE RELATIVE 0 FROM zero_position", &[])
            .unwrap()
            .affected_rows,
        1
    );
    let refetch = engine
        .sql("FETCH RELATIVE 0 FROM zero_position", &[])
        .unwrap_err();
    assert_eq!(refetch.sqlstate(), Some("55000"), "{refetch}");
    engine.sql("ROLLBACK", &[]).unwrap();

    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql(
            "DECLARE negative_absolute NO SCROLL CURSOR FOR SELECT x FROM (VALUES (1), (2)) AS rows(x) ORDER BY x",
            &[],
        )
        .unwrap();
    let backward_absolute = engine
        .sql("FETCH ABSOLUTE -1 FROM negative_absolute", &[])
        .unwrap_err();
    assert_eq!(
        backward_absolute.sqlstate(),
        Some("55000"),
        "{backward_absolute}"
    );
    engine.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn pg18_sql_cursor_no_scroll_and_locking_rules_match_postgresql() {
    let engine = Engine::new();
    engine
        .sql("CREATE TABLE cursor_lock_rows (x INTEGER)", &[])
        .unwrap();
    engine
        .sql("INSERT INTO cursor_lock_rows VALUES (1), (2)", &[])
        .unwrap();
    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql(
            "DECLARE exhausted NO SCROLL CURSOR FOR SELECT x FROM (VALUES (1), (2)) AS rows(x) ORDER BY x",
            &[],
        )
        .unwrap();
    engine.sql("FETCH 3 FROM exhausted", &[]).unwrap();
    assert!(engine
        .sql("FETCH FORWARD 0 FROM exhausted", &[])
        .unwrap()
        .rows
        .is_empty());
    assert_eq!(
        engine
            .sql("MOVE FORWARD 0 FROM exhausted", &[])
            .unwrap()
            .affected_rows,
        0
    );
    engine.sql("ROLLBACK", &[]).unwrap();

    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql(
            "DECLARE forward_only NO SCROLL CURSOR FOR SELECT x FROM (VALUES (1), (2), (3)) AS rows(x) ORDER BY x",
            &[],
        )
        .unwrap();
    engine.sql("FETCH 2 FROM forward_only", &[]).unwrap();
    let backward = engine
        .sql("FETCH BACKWARD FROM forward_only", &[])
        .unwrap_err();
    assert_eq!(backward.sqlstate(), Some("55000"), "{backward}");
    assert_eq!(backward.to_string(), "cursor can only scan forward");
    engine.sql("ROLLBACK", &[]).unwrap();

    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql(
            "DECLARE locking_cursor CURSOR FOR SELECT x FROM cursor_lock_rows ORDER BY x FOR UPDATE",
            &[],
        )
        .unwrap();
    engine.sql("FETCH FROM locking_cursor", &[]).unwrap();
    let backward = engine
        .sql("FETCH BACKWARD FROM locking_cursor", &[])
        .unwrap_err();
    assert_eq!(backward.sqlstate(), Some("55000"), "{backward}");
    engine.sql("ROLLBACK", &[]).unwrap();

    for sql in [
        "DECLARE held_lock CURSOR WITH HOLD FOR SELECT x FROM cursor_lock_rows FOR UPDATE",
        "DECLARE scrolling_lock SCROLL CURSOR FOR SELECT x FROM cursor_lock_rows FOR UPDATE",
    ] {
        engine.sql("BEGIN", &[]).unwrap();
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("0A000"), "{sql}: {error}");
        engine.sql("ROLLBACK", &[]).unwrap();
    }
}

#[test]
fn pg18_projection_of_an_unknown_column_reports_undefined_column() {
    let eng = Engine::new();
    let error = eng.sql("SELECT trans_foo", &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42703"), "{error}");
}

#[test]
fn pg18_plpgsql_return_accepts_query_shaped_expressions() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE plpgsql_return_source (a SMALLINT)", &[])
        .unwrap();
    eng.sql("INSERT INTO plpgsql_return_source VALUES (4), (9)", &[])
        .unwrap();
    eng.sql(
        "CREATE FUNCTION max_plpgsql_return_source() RETURNS SMALLINT LANGUAGE plpgsql AS 'BEGIN RETURN max(a) FROM plpgsql_return_source; END' STABLE",
        &[],
    )
    .unwrap();
    let result = eng
        .sql("SELECT max_plpgsql_return_source() AS maximum", &[])
        .unwrap();
    assert_eq!(result.rows[0]["maximum"], uqa_core::Value::Int(9));
}

#[test]
fn pg18_stable_and_volatile_routines_use_statement_and_command_visibility() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE routine_visibility (a SMALLINT, ordering INTEGER); INSERT INTO routine_visibility VALUES (56, 1), (100, 2), (0, 3), (42, 4), (777, 5)",
        &[],
    )
    .unwrap();

    for (language, body) in [
        ("sql", "SELECT max(a) FROM routine_visibility"),
        (
            "plpgsql",
            "BEGIN RETURN max(a) FROM routine_visibility; END",
        ),
    ] {
        eng.sql(
            &format!(
                "CREATE OR REPLACE FUNCTION max_routine_visibility() RETURNS SMALLINT LANGUAGE {language} AS '{body}' STABLE"
            ),
            &[],
        )
        .unwrap();
        eng.sql("BEGIN", &[]).unwrap();
        eng.sql(
            "UPDATE routine_visibility SET a = max_routine_visibility() + 10 WHERE a > 0",
            &[],
        )
        .unwrap();
        let stable = eng
            .sql("SELECT a FROM routine_visibility ORDER BY ordering", &[])
            .unwrap();
        assert_eq!(integer_column(&stable, "a"), [787, 787, 0, 787, 787]);
        eng.sql("ROLLBACK", &[]).unwrap();

        eng.sql(
            &format!(
                "CREATE OR REPLACE FUNCTION max_routine_visibility() RETURNS SMALLINT LANGUAGE {language} AS '{body}' VOLATILE"
            ),
            &[],
        )
        .unwrap();
        eng.sql("BEGIN", &[]).unwrap();
        eng.sql(
            "UPDATE routine_visibility SET a = max_routine_visibility() + 10 WHERE a > 0",
            &[],
        )
        .unwrap();
        let volatile = eng
            .sql("SELECT a FROM routine_visibility ORDER BY ordering", &[])
            .unwrap();
        assert_eq!(integer_column(&volatile, "a"), [787, 797, 0, 807, 817]);
        eng.sql("ROLLBACK", &[]).unwrap();
    }
}

#[test]
fn pg18_vacuum_runs_outside_transactions_and_preserves_error_precedence() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE vacuum_target (a INTEGER, b TEXT)", &[])
        .unwrap();
    eng.sql("INSERT INTO vacuum_target VALUES (1, 'one')", &[])
        .unwrap();
    eng.sql("VACUUM", &[]).unwrap();
    eng.sql(
        "VACUUM (ANALYZE, FULL, FREEZE, PARALLEL 0, BUFFER_USAGE_LIMIT '128 kB') vacuum_target (a)",
        &[],
    )
    .unwrap();
    eng.sql("VACUUM (BUFFER_USAGE_LIMIT '0.125 MB')", &[])
        .unwrap();
    eng.sql("VACUUM (ONLY_DATABASE_STATS, ANALYZE false)", &[])
        .unwrap();
    eng.sql("VACUUM (ONLY_DATABASE_STATS, BUFFER_USAGE_LIMIT 0)", &[])
        .unwrap();
    eng.sql(
        "VACUUM (ONLY_DATABASE_STATS, VERBOSE 0, PROCESS_MAIN FALSE, PROCESS_TOAST FALSE, INDEX_CLEANUP AUTO, TRUNCATE FALSE, PARALLEL 2, BUFFER_USAGE_LIMIT '128 kB')",
        &[],
    )
    .unwrap();
    eng.sql("VACUUM (VERBOSE 0) vacuum_target", &[]).unwrap();
    eng.sql("VACUUM (VERBOSE 1) vacuum_target", &[]).unwrap();
    eng.sql("VACUUM (ONLY_DATABASE_STATS false, ANALYZE)", &[])
        .unwrap();

    for (sql, message) in [
        (
            "VACUUM (FULL, PARALLEL 1)",
            "VACUUM FULL cannot be performed in parallel",
        ),
        (
            "VACUUM (ONLY_DATABASE_STATS, ANALYZE)",
            "ONLY_DATABASE_STATS cannot be specified with other VACUUM options",
        ),
        (
            "VACUUM (ONLY_DATABASE_STATS) vacuum_target",
            "ONLY_DATABASE_STATS cannot be specified with a list of tables",
        ),
        (
            "VACUUM (FULL, BUFFER_USAGE_LIMIT '128 kB') vacuum_target",
            "BUFFER_USAGE_LIMIT cannot be specified for VACUUM FULL",
        ),
        (
            "VACUUM (FULL, DISABLE_PAGE_SKIPPING) vacuum_target",
            "VACUUM option DISABLE_PAGE_SKIPPING cannot be used with FULL",
        ),
        (
            "VACUUM (FULL, PROCESS_TOAST FALSE) vacuum_target",
            "PROCESS_TOAST required with VACUUM FULL",
        ),
    ] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("0A000"), "{sql}: {error}");
        assert_eq!(error.to_string(), message, "{sql}");
    }

    let unknown_option = eng.sql("VACUUM (NOT_A_PG18_OPTION)", &[]).unwrap_err();
    assert_eq!(unknown_option.sqlstate(), Some("42601"), "{unknown_option}");
    let invalid_buffer = eng
        .sql("VACUUM (BUFFER_USAGE_LIMIT '127 kB')", &[])
        .unwrap_err();
    assert_eq!(invalid_buffer.sqlstate(), Some("22023"), "{invalid_buffer}");
    let invalid_boolean = eng.sql("VACUUM (VERBOSE 2)", &[]).unwrap_err();
    assert_eq!(
        invalid_boolean.sqlstate(),
        Some("42601"),
        "{invalid_boolean}"
    );
    let invalid_boolean_alias = eng.sql("VACUUM (ANALYZE yes)", &[]).unwrap_err();
    assert_eq!(
        invalid_boolean_alias.sqlstate(),
        Some("42601"),
        "{invalid_boolean_alias}"
    );
    let missing_table = eng.sql("VACUUM missing_vacuum_target", &[]).unwrap_err();
    assert_eq!(missing_table.sqlstate(), Some("42P01"), "{missing_table}");
    let columns_without_analyze = eng.sql("VACUUM vacuum_target (a)", &[]).unwrap_err();
    assert_eq!(
        columns_without_analyze.sqlstate(),
        Some("0A000"),
        "{columns_without_analyze}"
    );
    let missing_column = eng
        .sql("VACUUM (ANALYZE) vacuum_target (missing)", &[])
        .unwrap_err();
    assert_eq!(missing_column.sqlstate(), Some("42703"), "{missing_column}");

    eng.sql("BEGIN", &[]).unwrap();
    let inside = eng.sql("VACUUM (NOT_A_PG18_OPTION)", &[]).unwrap_err();
    assert_eq!(inside.sqlstate(), Some("25001"), "{inside}");
    assert!(inside
        .to_string()
        .contains("VACUUM cannot run inside a transaction block"));
    eng.sql("ROLLBACK", &[]).unwrap();

    let batch = eng.sql("VACUUM; SELECT 1", &[]).unwrap_err();
    assert_eq!(batch.sqlstate(), Some("25001"), "{batch}");
    assert_eq!(eng.transaction_depth(), 0);
}

#[test]
fn pg18_xmin_tracks_top_level_and_savepoint_tuple_versions() {
    let eng = Engine::new();
    for column in ["tableoid", "xmin", "cmin", "xmax", "cmax", "ctid"] {
        let error = eng
            .sql(
                &format!("CREATE TABLE reserved_{column} ({column} TEXT)"),
                &[],
            )
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("42701"), "{column}: {error}");
        assert_eq!(
            error.to_string(),
            format!("column name \"{column}\" conflicts with a system column name")
        );
    }
    eng.sql(
        "CREATE TABLE quoted_system_name (\"XMIN\" TEXT, value INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE SERVER system_column_server FOREIGN DATA WRAPPER duckdb_fdw OPTIONS (database ':memory:')",
        &[],
    )
    .unwrap();
    for sql in [
        "CREATE FOREIGN TABLE reserved_foreign_xmin (xmin INTEGER) SERVER system_column_server",
        "CREATE MATERIALIZED VIEW reserved_materialized_xmin AS SELECT 1 AS xmin WITH NO DATA",
    ] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42701"), "{sql}: {error}");
    }
    eng.sql(
        "CREATE FOREIGN TABLE quoted_foreign_xmin (\"XMIN\" INTEGER) SERVER system_column_server",
        &[],
    )
    .unwrap();
    eng.sql("CREATE VIEW ordinary_xmin_view AS SELECT 1 AS xmin", &[])
        .unwrap();
    for sql in [
        "ALTER TABLE quoted_system_name ADD COLUMN xmin TEXT",
        "ALTER TABLE quoted_system_name RENAME COLUMN value TO xmin",
    ] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42701"), "{sql}: {error}");
    }
    eng.sql("CREATE TABLE xacttest (a INTEGER)", &[]).unwrap();
    eng.add_document(
        "xacttest",
        99,
        Document::from([("a".into(), uqa_core::Value::Int(99))]),
    )
    .unwrap();
    let direct_xmin = integer_column(
        &eng.sql("SELECT xmin FROM xacttest WHERE a = 99", &[])
            .unwrap(),
        "xmin",
    );
    assert_eq!(direct_xmin.len(), 1);
    assert!(direct_xmin[0] > 0);
    eng.sql("BEGIN", &[]).unwrap();
    eng.sql("INSERT INTO xacttest VALUES (6)", &[]).unwrap();
    eng.sql("SAVEPOINT one", &[]).unwrap();
    eng.sql("INSERT INTO xacttest VALUES (7)", &[]).unwrap();
    eng.sql("RELEASE SAVEPOINT one", &[]).unwrap();
    eng.sql("INSERT INTO xacttest VALUES (8)", &[]).unwrap();
    eng.sql("COMMIT", &[]).unwrap();

    let released = eng
        .sql(
            "SELECT a.xmin = b.xmin AS same FROM xacttest a CROSS JOIN xacttest b WHERE a.a = 6 AND b.a = 8",
            &[],
        )
        .unwrap();
    assert_eq!(released.rows[0]["same"], uqa_core::Value::Bool(true));
    let child = eng
        .sql(
            "SELECT a.xmin = b.xmin AS same FROM xacttest a CROSS JOIN xacttest b WHERE a.a = 6 AND b.a = 7",
            &[],
        )
        .unwrap();
    assert_eq!(child.rows[0]["same"], uqa_core::Value::Bool(false));

    eng.sql("BEGIN", &[]).unwrap();
    eng.sql("INSERT INTO xacttest VALUES (9)", &[]).unwrap();
    eng.sql("SAVEPOINT one", &[]).unwrap();
    eng.sql("INSERT INTO xacttest VALUES (10)", &[]).unwrap();
    eng.sql("ROLLBACK TO SAVEPOINT one", &[]).unwrap();
    eng.sql("INSERT INTO xacttest VALUES (11)", &[]).unwrap();
    eng.sql("COMMIT", &[]).unwrap();

    let restarted = eng
        .sql(
            "SELECT a.xmin = b.xmin AS same FROM xacttest a CROSS JOIN xacttest b WHERE a.a = 9 AND b.a = 11",
            &[],
        )
        .unwrap();
    assert_eq!(restarted.rows[0]["same"], uqa_core::Value::Bool(false));
    assert!(eng
        .sql("SELECT a FROM xacttest WHERE a = 10", &[])
        .unwrap()
        .rows
        .is_empty());
}

#[test]
fn pg18_xmin_is_explicitly_addressable_but_hidden_from_stars_and_survives_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("xmin.db");
    let first_xmin = {
        let eng = Engine::open(&path).unwrap();
        eng.sql("CREATE TABLE versioned (a INTEGER)", &[]).unwrap();
        let inserted = eng
            .sql("INSERT INTO versioned VALUES (1) RETURNING xmin", &[])
            .unwrap();
        assert_eq!(inserted.columns, ["xmin"]);
        let star = eng.sql("SELECT * FROM versioned", &[]).unwrap();
        assert_eq!(star.columns, ["a"]);
        inserted.rows[0]["xmin"].clone()
    };

    let eng = Engine::open(&path).unwrap();
    let inserted = eng
        .sql("INSERT INTO versioned VALUES (2) RETURNING xmin", &[])
        .unwrap();
    assert_ne!(inserted.rows[0]["xmin"], first_xmin);
}

#[test]
fn savepoint_order_invalidates_descendants_and_preserves_shadowed_names() {
    let eng = Engine::new();
    eng.begin().unwrap();
    eng.savepoint("dup").unwrap();
    eng.sql("CREATE SCHEMA first_level", &[]).unwrap();
    eng.savepoint("dup").unwrap();
    eng.sql("CREATE SCHEMA second_level", &[]).unwrap();

    eng.rollback_to_savepoint("dup").unwrap();
    assert!(eng.has_schema("first_level").unwrap());
    assert!(!eng.has_schema("second_level").unwrap());
    eng.release_savepoint("dup").unwrap();
    eng.rollback_to_savepoint("dup").unwrap();
    assert!(!eng.has_schema("first_level").unwrap());

    eng.savepoint("outer").unwrap();
    eng.savepoint("inner").unwrap();
    eng.rollback_to_savepoint("outer").unwrap();
    assert!(eng.release_savepoint("inner").is_err());
    // PostgreSQL 18: a failed savepoint command aborts the transaction, so commands report 25P02 until ROLLBACK TO an earlier savepoint clears it.
    let error = eng.release_savepoint("outer").unwrap_err();
    assert_eq!(error.sqlstate(), Some("25P02"));
    eng.rollback_to_savepoint("outer").unwrap();
    eng.release_savepoint("outer").unwrap();
    eng.commit().unwrap();
    assert!(!eng.has_schema("first_level").unwrap());
}

#[test]
fn close_drops_open_transactions() {
    let eng = Engine::new();
    eng.begin().unwrap();
    eng.begin().unwrap();
    assert_eq!(eng.transaction_depth(), 2);
    eng.close().unwrap();
    assert_eq!(eng.transaction_depth(), 0);
}

#[test]
fn commit_without_begin_errors() {
    let eng = Engine::new();
    assert!(eng.commit().is_err());
}

#[test]
fn sql_commit_and_rollback_without_begin_warn_instead_of_erroring() {
    let eng = Engine::new();
    eng.sql("COMMIT", &[]).unwrap();
    eng.sql("ROLLBACK", &[]).unwrap();
    assert_eq!(
        eng.take_sql_notices(),
        vec![
            (
                "WARNING".into(),
                "there is no transaction in progress".into()
            ),
            (
                "WARNING".into(),
                "there is no transaction in progress".into()
            ),
        ]
    );
}

#[test]
fn side_effecting_selects_use_statement_rollback_in_memory() {
    let eng = Engine::new();

    // The inner graph function must run before the selected CASE branch
    // divides by zero. The failed SELECT must still remove the graph it created.
    let graph_error = eng
        .sql(
            "SELECT CASE WHEN graph_create('transient_graph')
                    THEN 1 / 0 ELSE 0 END",
            &[],
        )
        .unwrap_err();
    assert!(!graph_error.to_string().is_empty());
    assert!(!eng.has_graph("transient_graph").unwrap());

    // Mutating table functions live in SourcePlan rather than ScalarExpr.
    // Their source row feeds the failing cast, proving creation happened
    // before the outer projection failed and was rolled back.
    let analyzer_error = eng
        .sql(
            "SELECT CAST(created AS INTEGER)
             FROM create_analyzer(
                'transient_analyzer',
                '{\"tokenizer\":\"keyword\"}'
             ) AS made(created)",
            &[],
        )
        .unwrap_err();
    assert!(analyzer_error
        .to_string()
        .to_ascii_lowercase()
        .contains("integer"));
    assert!(!eng
        .list_named_analyzers()
        .unwrap()
        .contains(&"transient_analyzer".to_string()));

    // Scalar mutators nested under both a subquery source and a CTE must be
    // discovered by the same classifier.
    eng.sql("CREATE TABLE udf_rows (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    let nested_error = eng
        .sql(
            "WITH nested AS (
                SELECT changed
                FROM (SELECT graph_create('nested_graph') AS changed) AS child
             )
             SELECT CASE WHEN changed THEN 1 / 0 ELSE 0 END FROM nested",
            &[],
        )
        .unwrap_err();
    assert!(!nested_error.to_string().is_empty());
    assert!(!eng.has_graph("nested_graph").unwrap());

    // A registered routine can hide DML behind an ordinary Func node. The
    // outer type error happens after the INSERT and must roll it back.
    eng.sql(
        "CREATE FUNCTION mutate_then_return() RETURNS INTEGER AS $$
         BEGIN
           INSERT INTO udf_rows (id) VALUES (1);
           RETURN 1;
         END;
         $$ LANGUAGE plpgsql",
        &[],
    )
    .unwrap();
    eng.sql(
        "SELECT CASE WHEN mutate_then_return() = 1 THEN 1 / 0 ELSE 0 END",
        &[],
    )
    .unwrap_err();
    let count = eng.sql("SELECT count(*) AS n FROM udf_rows", &[]).unwrap();
    assert_eq!(count.rows[0]["n"], uqa_core::Value::Int(0));

    // cypher() can mutate an existing graph from a SourcePlan::Function.
    // Returning the created property makes the outer cast fail only after
    // the graph write has happened.
    eng.sql("SELECT create_graph('cypher_tx')", &[]).unwrap();
    eng.sql(
        "SELECT CAST(name AS INTEGER)
         FROM cypher('cypher_tx', $$
            CREATE (n:Person {name: 'not-an-integer'})
            RETURN n.name
         $$) AS (name text)",
        &[],
    )
    .unwrap_err();
    let cypher_rows = eng
        .sql(
            "SELECT * FROM cypher('cypher_tx', $$
                MATCH (n:Person) RETURN n.name
             $$) AS (name text)",
            &[],
        )
        .unwrap();
    assert!(cypher_rows.rows.is_empty());

    // PostgreSQL's per-session RNG is nontransactional. A failed outer
    // expression leaves each draw consumed even though SQL mutations roll back.
    assert_failed_random_draws_remain_consumed(&eng);
}

fn assert_failed_random_draws_remain_consumed(eng: &Engine) {
    let baseline = Engine::new();
    baseline.sql("SELECT setseed(0.25)", &[]).unwrap();
    baseline.sql("SELECT random()", &[]).unwrap();
    let expected = baseline.sql("SELECT random() AS value", &[]).unwrap();
    eng.sql("SELECT setseed(0.25)", &[]).unwrap();
    eng.sql("SELECT CASE WHEN random() >= 0 THEN 1 / 0 ELSE 0 END", &[])
        .unwrap_err();
    let actual = eng.sql("SELECT random() AS value", &[]).unwrap();
    assert_eq!(actual.rows[0]["value"], expected.rows[0]["value"]);

    let baseline = Engine::new();
    baseline.sql("SELECT setseed(0.25)", &[]).unwrap();
    baseline
        .sql("SELECT random(-10::bigint, 10::bigint)", &[])
        .unwrap();
    let expected = baseline.sql("SELECT random() AS value", &[]).unwrap();
    eng.sql("SELECT setseed(0.25)", &[]).unwrap();
    eng.sql(
        "SELECT CASE WHEN random(-10::bigint, 10::bigint) >= -10 THEN 1 / 0 ELSE 0 END",
        &[],
    )
    .unwrap_err();
    let actual = eng.sql("SELECT random() AS value", &[]).unwrap();
    assert_eq!(actual.rows[0]["value"], expected.rows[0]["value"]);
}

#[test]
fn side_effecting_select_rollback_matches_memory_and_catalog_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("side_effecting_select.db");

    {
        let eng = Engine::open(&path).unwrap();
        eng.sql(
            "SELECT CASE WHEN graph_create('transient_graph')
                    THEN 1 / 0 ELSE 0 END",
            &[],
        )
        .unwrap_err();
        assert!(!eng.has_graph("transient_graph").unwrap());
    }

    let reopened = Engine::open(&path).unwrap();
    assert!(!reopened.has_graph("transient_graph").unwrap());
}

#[test]
fn explicit_memory_rollback_restores_every_sql_owned_registry() {
    let eng = Engine::new();
    eng.sql("SET work_mem = '8MB'", &[]).unwrap();
    eng.create_graph("base_graph").unwrap();

    eng.sql("BEGIN", &[]).unwrap();
    eng.sql("CREATE SCHEMA tx_schema", &[]).unwrap();
    eng.sql("SET search_path TO tx_schema, public", &[])
        .unwrap();
    eng.sql("SET work_mem = '16MB'", &[]).unwrap();
    eng.sql("CREATE VIEW tx_view AS SELECT 1 AS n", &[])
        .unwrap();
    eng.sql("CREATE SEQUENCE tx_sequence", &[]).unwrap();
    eng.sql("PREPARE tx_prepared AS SELECT 1 AS n", &[])
        .unwrap();
    eng.sql(
        "CREATE FUNCTION tx_function() RETURNS INTEGER
         LANGUAGE SQL AS 'SELECT 7'",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE SERVER tx_server FOREIGN DATA WRAPPER memory_fdw
         OPTIONS (kind 'memory')",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE FOREIGN TABLE tx_remote (id INTEGER)
         SERVER tx_server OPTIONS (source 'memory')",
        &[],
    )
    .unwrap();
    eng.sql("SELECT graph_create('tx_graph')", &[]).unwrap();
    eng.sql(
        "SELECT * FROM create_analyzer(
            'tx_analyzer', '{\"tokenizer\":\"keyword\"}'
         )",
        &[],
    )
    .unwrap();
    eng.build_path_index("tx_path", "base_graph", &[vec!["knows".into()]])
        .unwrap();
    eng.save_scoring_params("tx_score", "{\"alpha\":1}")
        .unwrap();
    eng.save_model(
        "tx_model",
        &uqa_ml::DeepModel {
            layers: Vec::new(),
            alpha: 0.0,
            gating: uqa_ml::GatingSpec::None,
        },
    )
    .unwrap();
    eng.sql("ROLLBACK", &[]).unwrap();

    assert!(!eng.list_schemas().unwrap().contains(&"tx_schema".into()));
    assert!(!eng.list_views().unwrap().contains(&"tx_view".into()));
    assert!(!eng.has_graph("tx_graph").unwrap());
    assert!(!eng
        .list_named_analyzers()
        .unwrap()
        .contains(&"tx_analyzer".into()));
    assert!(!eng
        .list_foreign_servers()
        .unwrap()
        .contains(&"tx_server".into()));
    assert!(!eng
        .list_foreign_tables()
        .unwrap()
        .contains(&"tx_remote".into()));
    assert!(eng.list_path_indexes().unwrap().is_empty());
    assert!(eng.load_scoring_params("tx_score").unwrap().is_none());
    assert!(eng.load_model("tx_model").unwrap().is_none());
    assert!(eng.sql("SELECT tx_function()", &[]).is_err());
    assert!(eng.sql("EXECUTE tx_prepared", &[]).is_err());
    assert!(eng.currval("tx_sequence").is_err());

    let work_mem = eng.sql("SHOW work_mem", &[]).unwrap();
    assert_eq!(
        work_mem.rows[0]["work_mem"],
        uqa_core::Value::Str("8MB".into())
    );
    assert_eq!(eng.search_path(), vec!["public"]);
}

#[test]
fn memory_savepoint_restores_registry_state_without_losing_earlier_changes() {
    let eng = Engine::new();
    eng.sql("BEGIN", &[]).unwrap();
    eng.sql("SELECT graph_create('before_savepoint')", &[])
        .unwrap();
    eng.sql("SAVEPOINT registry_point", &[]).unwrap();
    eng.sql("SELECT graph_create('after_savepoint')", &[])
        .unwrap();
    eng.sql("CREATE SCHEMA after_savepoint_schema", &[])
        .unwrap();

    eng.sql("ROLLBACK TO SAVEPOINT registry_point", &[])
        .unwrap();
    assert!(eng.has_graph("before_savepoint").unwrap());
    assert!(!eng.has_graph("after_savepoint").unwrap());
    assert!(!eng
        .list_schemas()
        .unwrap()
        .contains(&"after_savepoint_schema".into()));
    eng.sql("COMMIT", &[]).unwrap();
    assert!(eng.has_graph("before_savepoint").unwrap());
}

#[test]
fn persistent_rollback_restores_transactional_session_state_but_not_random_state() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("session-state.db")).unwrap();
    engine.sql("CREATE SCHEMA base", &[]).unwrap();
    engine.sql("SET search_path TO base, public", &[]).unwrap();
    engine.sql("SET work_mem = '8MB'", &[]).unwrap();
    engine.sql("SELECT setseed(0.25)", &[]).unwrap();

    let baseline = Engine::new();
    baseline.sql("SELECT setseed(-0.5)", &[]).unwrap();
    let expected_outer = baseline.sql("SELECT random() AS value", &[]).unwrap();

    engine.sql("BEGIN", &[]).unwrap();
    engine.sql("SET search_path TO public", &[]).unwrap();
    engine.sql("SET work_mem = '16MB'", &[]).unwrap();
    engine.sql("SELECT setseed(-0.5)", &[]).unwrap();
    engine
        .sql("PREPARE rolled_back AS SELECT 1 AS value", &[])
        .unwrap();
    engine.sql("ROLLBACK", &[]).unwrap();

    assert_eq!(engine.search_path(), vec!["base", "public"]);
    assert_eq!(
        engine.sql("SHOW work_mem", &[]).unwrap().rows[0]["work_mem"],
        uqa_core::Value::Str("8MB".into())
    );
    assert!(engine.sql("EXECUTE rolled_back", &[]).is_err());
    assert_eq!(
        engine.sql("SELECT random() AS value", &[]).unwrap().rows[0]["value"],
        expected_outer.rows[0]["value"]
    );

    engine.sql("BEGIN", &[]).unwrap();
    engine.sql("SET work_mem = '12MB'", &[]).unwrap();
    engine.sql("SELECT setseed(0.5)", &[]).unwrap();
    engine.sql("SAVEPOINT session_point", &[]).unwrap();

    let savepoint_baseline = Engine::new();
    savepoint_baseline
        .sql("SELECT setseed(-0.75)", &[])
        .unwrap();
    let expected_savepoint = savepoint_baseline
        .sql("SELECT random() AS value", &[])
        .unwrap();

    engine.sql("SET search_path TO public", &[]).unwrap();
    engine.sql("SET work_mem = '20MB'", &[]).unwrap();
    engine.sql("SELECT setseed(-0.75)", &[]).unwrap();
    engine
        .sql("PREPARE after_savepoint AS SELECT 2 AS value", &[])
        .unwrap();
    engine
        .sql("ROLLBACK TO SAVEPOINT session_point", &[])
        .unwrap();

    assert_eq!(engine.search_path(), vec!["base", "public"]);
    assert_eq!(
        engine.sql("SHOW work_mem", &[]).unwrap().rows[0]["work_mem"],
        uqa_core::Value::Str("12MB".into())
    );
    let execute_error = engine.sql("EXECUTE after_savepoint", &[]).unwrap_err();
    let aborted = engine.sql("SELECT 1", &[]).unwrap_err();
    assert_eq!(aborted.sqlstate(), Some("25P02"), "{execute_error}");
    engine
        .sql("ROLLBACK TO SAVEPOINT session_point", &[])
        .unwrap();
    assert_eq!(
        engine.sql("SELECT random() AS value", &[]).unwrap().rows[0]["value"],
        expected_savepoint.rows[0]["value"]
    );
    engine.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn persistent_rollback_rebinds_physical_analyzers_from_durable_registry() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("analyzer-rollback.db")).unwrap();
    engine
        .sql(
            "CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT); \
             CREATE INDEX docs_body_fts ON docs USING gin (body); \
             INSERT INTO docs (id, body) VALUES (1, 'hello world')",
            &[],
        )
        .unwrap();
    engine
        .register_named_analyzer("whole_value", r#"{"tokenizer":"keyword"}"#)
        .unwrap();
    assert_eq!(
        engine
            .sql("SELECT id FROM docs WHERE text_match(body, 'hello')", &[])
            .unwrap()
            .rows
            .len(),
        1
    );

    engine.begin().unwrap();
    engine
        .set_table_field_analyzer("docs", "body", "whole_value", "both")
        .unwrap();
    assert!(engine
        .sql("SELECT id FROM docs WHERE text_match(body, 'hello')", &[])
        .unwrap()
        .rows
        .is_empty());
    engine.rollback().unwrap();

    assert_eq!(engine.table_field_analyzer("docs", "body").unwrap(), None);
    assert_eq!(
        engine
            .sql("SELECT id FROM docs WHERE text_match(body, 'hello')", &[])
            .unwrap()
            .rows
            .len(),
        1
    );
}
