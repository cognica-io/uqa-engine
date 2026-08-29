//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Engine-level transaction lifecycle convenience methods for begin, commit,
//! rollback, and savepoint operations.

use uqa_engine::Engine;
use uqa_sql::ast::ColumnType;
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

fn assert_integer_query<const N: usize>(
    engine: &Engine,
    sql: &str,
    column: &str,
    expected: [i64; N],
) {
    assert_eq!(
        integer_column(&engine.sql(sql, &[]).unwrap(), column),
        expected
    );
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

    eng.sql("BEGIN", &[]).unwrap();
    eng.sql("INSERT INTO transaction_mode_rows VALUES (2)", &[])
        .unwrap();
    eng.sql("SET TRANSACTION READ ONLY", &[]).unwrap();
    eng.sql("ANALYZE transaction_mode_rows", &[]).unwrap();
    eng.sql("COMMIT", &[]).unwrap();
    assert_eq!(
        integer_column(
            &eng.sql("SELECT id FROM transaction_mode_rows ORDER BY id", &[])
                .unwrap(),
            "id"
        ),
        [1, 2]
    );

    eng.sql("PREPARE transaction_probe AS SELECT 1", &[])
        .unwrap();
    eng.sql("BEGIN", &[]).unwrap();
    eng.sql("DEALLOCATE transaction_probe", &[]).unwrap();
    eng.sql("SET TRANSACTION ISOLATION LEVEL SERIALIZABLE", &[])
        .unwrap();
    eng.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn pg18_prepare_acquires_a_transaction_snapshot() {
    let eng = Engine::new();
    eng.sql("BEGIN", &[]).unwrap();
    eng.sql("PREPARE snapshot_probe AS SELECT 1", &[]).unwrap();
    let error = eng
        .sql("SET TRANSACTION ISOLATION LEVEL SERIALIZABLE", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("25001"), "{error}");
    eng.sql("ROLLBACK", &[]).unwrap();
}

fn assert_basic_persistent_isolation_snapshots(root: &Engine, sibling: &Engine) {
    root.sql("BEGIN ISOLATION LEVEL READ COMMITTED", &[])
        .unwrap();
    assert_integer_query(
        root,
        "SELECT value FROM isolation_rows WHERE id = 1",
        "value",
        [10],
    );
    sibling
        .sql("UPDATE isolation_rows SET value = 20 WHERE id = 1", &[])
        .unwrap();
    assert_integer_query(
        root,
        "SELECT value FROM isolation_rows WHERE id = 1",
        "value",
        [20],
    );
    root.sql("ROLLBACK", &[]).unwrap();

    root.sql("BEGIN ISOLATION LEVEL REPEATABLE READ", &[])
        .unwrap();
    assert_integer_query(
        root,
        "SELECT value FROM isolation_rows WHERE id = 1",
        "value",
        [20],
    );
    sibling
        .sql("UPDATE isolation_rows SET value = 30 WHERE id = 1", &[])
        .unwrap();
    assert_integer_query(
        root,
        "SELECT value FROM isolation_rows WHERE id = 1",
        "value",
        [20],
    );
    let error = root
        .sql("UPDATE isolation_rows SET value = 31 WHERE id = 1", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("40001"), "{error}");
    root.sql("ROLLBACK", &[]).unwrap();

    root.sql("BEGIN ISOLATION LEVEL SERIALIZABLE", &[]).unwrap();
    assert_integer_query(
        root,
        "SELECT value FROM isolation_rows WHERE id = 1",
        "value",
        [30],
    );
    sibling
        .sql("UPDATE isolation_rows SET value = 40 WHERE id = 1", &[])
        .unwrap();
    let error = root
        .sql("UPDATE isolation_rows SET value = 41 WHERE id = 1", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("40001"), "{error}");
    root.sql("ROLLBACK", &[]).unwrap();

    root.sql("BEGIN ISOLATION LEVEL REPEATABLE READ", &[])
        .unwrap();
    sibling
        .sql("UPDATE isolation_rows SET value = 50 WHERE id = 1", &[])
        .unwrap();
    assert_integer_query(
        root,
        "SELECT value FROM isolation_rows WHERE id = 1",
        "value",
        [50],
    );
    root.sql("ROLLBACK", &[]).unwrap();
}

fn assert_cursor_and_nested_persistent_snapshots(root: &Engine, sibling: &Engine) {
    root.sql("BEGIN ISOLATION LEVEL READ COMMITTED", &[])
        .unwrap();
    root.sql(
        "DECLARE declaration_snapshot CURSOR FOR SELECT id FROM isolation_rows ORDER BY id",
        &[],
    )
    .unwrap();
    sibling
        .sql("INSERT INTO isolation_rows VALUES (2, 200)", &[])
        .unwrap();
    assert_integer_query(root, "FETCH ALL FROM declaration_snapshot", "id", [1]);
    root.sql("ROLLBACK", &[]).unwrap();

    root.sql("BEGIN ISOLATION LEVEL REPEATABLE READ", &[])
        .unwrap();
    assert_integer_query(
        root,
        "SELECT id FROM isolation_rows ORDER BY id",
        "id",
        [1, 2],
    );
    sibling
        .sql("INSERT INTO isolation_rows VALUES (3, 300)", &[])
        .unwrap();
    root.sql("UPDATE isolation_rows SET value = 51 WHERE id = 1", &[])
        .unwrap();
    assert_integer_query(
        root,
        "SELECT id FROM isolation_rows ORDER BY id",
        "id",
        [1, 2],
    );
    assert_integer_query(
        root,
        "SELECT value FROM isolation_rows WHERE id = 1",
        "value",
        [51],
    );
    root.sql("ROLLBACK", &[]).unwrap();

    root.sql("BEGIN ISOLATION LEVEL REPEATABLE READ", &[])
        .unwrap();
    root.begin().unwrap();
    assert_integer_query(
        root,
        "SELECT value FROM isolation_rows WHERE id = 1",
        "value",
        [50],
    );
    sibling
        .sql("UPDATE isolation_rows SET value = 60 WHERE id = 1", &[])
        .unwrap();
    root.rollback().unwrap();
    assert_integer_query(
        root,
        "SELECT value FROM isolation_rows WHERE id = 1",
        "value",
        [50],
    );
    root.sql("ROLLBACK", &[]).unwrap();

    root.sql("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY", &[])
        .unwrap();
    assert_integer_query(
        root,
        "SELECT value FROM isolation_rows WHERE id = 1",
        "value",
        [60],
    );
    sibling
        .sql("UPDATE isolation_rows SET value = 70 WHERE id = 1", &[])
        .unwrap();
    root.sql("ANALYZE isolation_rows", &[]).unwrap();
    assert_integer_query(
        root,
        "SELECT value FROM isolation_rows WHERE id = 1",
        "value",
        [60],
    );
    root.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn pg18_persistent_transaction_snapshots_follow_isolation_level() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("isolation.db")).unwrap();
    root.sql(
        "CREATE TABLE isolation_rows (id INTEGER PRIMARY KEY, value INTEGER); INSERT INTO isolation_rows VALUES (1, 10)",
        &[],
    )
    .unwrap();
    let sibling = root.new_session().unwrap();

    root.sql("BEGIN READ ONLY", &[]).unwrap();
    root.sql("ANALYZE isolation_rows", &[]).unwrap();
    root.sql("COMMIT", &[]).unwrap();
    assert_basic_persistent_isolation_snapshots(&root, &sibling);
    assert_cursor_and_nested_persistent_snapshots(&root, &sibling);
}

#[test]
fn pg18_fixed_snapshot_tracks_transactional_relation_lifetimes_and_renames() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("fixed-snapshot-ddl.db")).unwrap();
    root.sql(
        "CREATE TABLE snapshot_ddl_rows (id INTEGER PRIMARY KEY); INSERT INTO snapshot_ddl_rows VALUES (1)",
        &[],
    )
    .unwrap();
    let sibling = root.new_session().unwrap();

    root.sql("BEGIN ISOLATION LEVEL REPEATABLE READ", &[])
        .unwrap();
    assert_eq!(
        integer_column(
            &root
                .sql("SELECT id FROM snapshot_ddl_rows ORDER BY id", &[])
                .unwrap(),
            "id"
        ),
        [1]
    );
    sibling
        .sql("INSERT INTO snapshot_ddl_rows VALUES (2)", &[])
        .unwrap();
    root.sql("UPDATE snapshot_ddl_rows SET id = 10 WHERE id = 1", &[])
        .unwrap();
    root.sql(
        "ALTER TABLE snapshot_ddl_rows RENAME TO snapshot_renamed_rows",
        &[],
    )
    .unwrap();
    assert_eq!(
        integer_column(
            &root
                .sql("SELECT id FROM snapshot_renamed_rows ORDER BY id", &[])
                .unwrap(),
            "id"
        ),
        [10]
    );
    root.sql("ROLLBACK", &[]).unwrap();

    root.sql("BEGIN ISOLATION LEVEL REPEATABLE READ", &[])
        .unwrap();
    assert_eq!(
        integer_column(
            &root
                .sql("SELECT id FROM snapshot_ddl_rows ORDER BY id", &[])
                .unwrap(),
            "id"
        ),
        [1, 2]
    );
    root.sql("DROP TABLE snapshot_ddl_rows", &[]).unwrap();
    root.sql(
        "CREATE TABLE snapshot_ddl_rows (id INTEGER PRIMARY KEY)",
        &[],
    )
    .unwrap();
    root.sql("INSERT INTO snapshot_ddl_rows VALUES (99)", &[])
        .unwrap();
    assert_eq!(
        integer_column(
            &root
                .sql("SELECT id FROM snapshot_ddl_rows ORDER BY id", &[])
                .unwrap(),
            "id"
        ),
        [99]
    );
    root.sql("ROLLBACK", &[]).unwrap();
    assert_eq!(
        integer_column(
            &root
                .sql("SELECT id FROM snapshot_ddl_rows ORDER BY id", &[])
                .unwrap(),
            "id"
        ),
        [1, 2]
    );

    root.sql("BEGIN ISOLATION LEVEL REPEATABLE READ", &[])
        .unwrap();
    assert_integer_query(
        &root,
        "SELECT id FROM snapshot_ddl_rows ORDER BY id",
        "id",
        [1, 2],
    );
    root.sql("TRUNCATE snapshot_ddl_rows", &[]).unwrap();
    assert_integer_query(&root, "SELECT id FROM snapshot_ddl_rows", "id", []);
    root.sql("ROLLBACK", &[]).unwrap();
    assert_integer_query(
        &root,
        "SELECT id FROM snapshot_ddl_rows ORDER BY id",
        "id",
        [1, 2],
    );
}

#[test]
fn pg18_fixed_snapshot_uses_current_catalog_with_snapshot_visible_rows() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("fixed-snapshot-catalog.db");
    let root = Engine::open(&database).unwrap();
    root.sql(
        "CREATE TABLE catalog_anchor (id INTEGER); INSERT INTO catalog_anchor VALUES (1); CREATE TABLE catalog_old_source (id INTEGER); INSERT INTO catalog_old_source VALUES (10); CREATE TABLE catalog_new_source (id INTEGER); INSERT INTO catalog_new_source VALUES (20); CREATE TABLE catalog_truncate_source (id INTEGER); INSERT INTO catalog_truncate_source VALUES (30); CREATE TABLE catalog_evolving_source (id INTEGER); INSERT INTO catalog_evolving_source VALUES (50); CREATE VIEW catalog_snapshot_view AS SELECT id FROM catalog_old_source; CREATE TEMP VIEW catalog_local_view AS SELECT id FROM catalog_old_source; CREATE TEMP SEQUENCE catalog_local_sequence START 9; CREATE FUNCTION catalog_snapshot_function() RETURNS INTEGER LANGUAGE SQL AS 'SELECT 1'",
        &[],
    )
    .unwrap();
    let sibling = Engine::open(&database).unwrap();

    root.sql("BEGIN ISOLATION LEVEL REPEATABLE READ", &[])
        .unwrap();
    assert_integer_query(&root, "SELECT id FROM catalog_anchor", "id", [1]);
    sibling
        .sql(
            "CREATE OR REPLACE VIEW catalog_snapshot_view AS SELECT id FROM catalog_new_source; CREATE OR REPLACE FUNCTION catalog_snapshot_function() RETURNS INTEGER LANGUAGE SQL AS 'SELECT 2'; CREATE TABLE catalog_created_after_snapshot (id INTEGER); INSERT INTO catalog_created_after_snapshot VALUES (40); TRUNCATE catalog_truncate_source",
            &[],
        )
        .unwrap();

    assert_integer_query(&root, "SELECT id FROM catalog_snapshot_view", "id", [20]);
    assert_integer_query(&root, "SELECT id FROM catalog_local_view", "id", [10]);
    assert_integer_query(
        &root,
        "SELECT nextval('catalog_local_sequence') AS value",
        "value",
        [9],
    );
    assert_integer_query(
        &root,
        "SELECT catalog_snapshot_function() AS value",
        "value",
        [2],
    );
    assert_integer_query(
        &root,
        "SELECT id FROM catalog_created_after_snapshot",
        "id",
        [],
    );
    assert_integer_query(&root, "SELECT id FROM catalog_truncate_source", "id", []);

    sibling
        .sql(
            "ALTER TABLE catalog_evolving_source ADD COLUMN marker INTEGER DEFAULT 7; ALTER TABLE catalog_evolving_source RENAME TO catalog_renamed_source",
            &[],
        )
        .unwrap();
    let evolved = root
        .sql(
            "SELECT id, marker FROM catalog_renamed_source ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(integer_column(&evolved, "id"), [50]);
    assert_eq!(integer_column(&evolved, "marker"), [7]);
    assert_integer_query(&root, "SELECT id FROM catalog_anchor", "id", [1]);
    root.sql("ROLLBACK", &[]).unwrap();
    sibling
        .sql("DROP TABLE catalog_renamed_source", &[])
        .unwrap();
    assert_integer_query(&root, "SELECT id FROM catalog_anchor", "id", [1]);
    let dropped = root
        .sql(
            "SELECT to_regclass('catalog_evolving_source') AS old_name, to_regclass('catalog_renamed_source') AS renamed_name",
            &[],
        )
        .unwrap();
    assert_eq!(dropped.rows[0]["old_name"], uqa_core::Value::Null);
    assert_eq!(dropped.rows[0]["renamed_name"], uqa_core::Value::Null);
}

#[test]
fn pg18_fixed_snapshot_ddl_validates_and_backfills_current_rows() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("fixed-snapshot-ddl-validation.db");
    let root = Engine::open(&database).unwrap();
    root.sql(
        "CREATE TABLE ddl_validation_anchor (id INTEGER); INSERT INTO ddl_validation_anchor VALUES (1)",
        &[],
    )
    .unwrap();
    let sibling = Engine::open(&database).unwrap();

    root.sql("BEGIN ISOLATION LEVEL REPEATABLE READ", &[])
        .unwrap();
    assert_integer_query(&root, "SELECT id FROM ddl_validation_anchor", "id", [1]);
    sibling
        .sql(
            "CREATE TABLE ddl_validation_later (id INTEGER); INSERT INTO ddl_validation_later VALUES (1)",
            &[],
        )
        .unwrap();
    assert_integer_query(
        &root,
        "SELECT count(*) AS value FROM ddl_validation_later",
        "value",
        [0],
    );
    let error = root
        .sql(
            "ALTER TABLE ddl_validation_later ADD COLUMN marker INTEGER NOT NULL",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("23502"), "{error}");
    root.sql("ROLLBACK", &[]).unwrap();
    assert_eq!(
        sibling.try_table_columns("ddl_validation_later").unwrap(),
        ["id"]
    );

    root.sql("BEGIN ISOLATION LEVEL REPEATABLE READ", &[])
        .unwrap();
    assert_integer_query(&root, "SELECT id FROM ddl_validation_anchor", "id", [1]);
    sibling
        .sql(
            "CREATE TABLE ddl_backfill_later (id INTEGER); INSERT INTO ddl_backfill_later VALUES (1)",
            &[],
        )
        .unwrap();
    assert_integer_query(
        &root,
        "SELECT count(*) AS value FROM ddl_backfill_later",
        "value",
        [0],
    );
    root.sql(
        "ALTER TABLE ddl_backfill_later ADD COLUMN marker INTEGER NOT NULL DEFAULT 7",
        &[],
    )
    .unwrap();
    root.sql("COMMIT", &[]).unwrap();
    assert_integer_query(
        &sibling,
        "SELECT marker FROM ddl_backfill_later",
        "marker",
        [7],
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
fn pg18_read_only_transactions_reject_typed_catalog_and_graph_mutations() {
    let directory = tempfile::tempdir().unwrap();
    let persistent_path = directory.path().join("typed-read-only.db");
    for eng in [Engine::new(), Engine::open(&persistent_path).unwrap()] {
        eng.sql("BEGIN READ ONLY", &[]).unwrap();
        let error = eng.save_scoring_params("forbidden", "{}").unwrap_err();
        assert_eq!(error.sqlstate(), Some("25006"), "{error}");
        assert_eq!(eng.load_scoring_params("forbidden").unwrap(), None);
        let error = eng
            .run_cypher(
                "forbidden_graph",
                "CREATE (n)",
                std::collections::BTreeMap::new(),
            )
            .unwrap_err();
        assert!(
            error.to_string().contains("read-only transaction"),
            "{error}"
        );
        eng.sql("ROLLBACK", &[]).unwrap();

        eng.sql("SET default_transaction_read_only = on", &[])
            .unwrap();
        let error = eng.save_scoring_params("forbidden", "{}").unwrap_err();
        assert_eq!(error.sqlstate(), Some("25006"), "{error}");
        assert_eq!(eng.load_scoring_params("forbidden").unwrap(), None);
        eng.sql("SET default_transaction_read_only = off", &[])
            .unwrap();
    }
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
fn pg18_sql_cursor_executes_on_first_fetch_and_uses_bounded_portal_storage() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE lazy_cursor_error (divisor INTEGER); INSERT INTO lazy_cursor_error VALUES (0); CREATE SEQUENCE lazy_cursor_sequence",
            &[],
        )
        .unwrap();
    assert_eq!(
        integer_column(
            &engine
                .sql("SELECT nextval('lazy_cursor_sequence') AS value", &[])
                .unwrap(),
            "value"
        ),
        [1]
    );

    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql(
            "DECLARE lazy_binary BINARY SCROLL CURSOR FOR SELECT nextval('lazy_cursor_sequence') AS value",
            &[],
        )
        .unwrap();
    assert_eq!(
        integer_column(
            &engine
                .sql("SELECT currval('lazy_cursor_sequence') AS value", &[])
                .unwrap(),
            "value"
        ),
        [1]
    );
    assert_eq!(
        integer_column(&engine.sql("FETCH FROM lazy_binary", &[]).unwrap(), "value"),
        [2]
    );
    engine.sql("CLOSE lazy_binary", &[]).unwrap();
    engine
        .sql(
            "DECLARE incremental_cursor CURSOR FOR SELECT nextval('lazy_cursor_sequence') AS value FROM generate_series(1, 3)",
            &[],
        )
        .unwrap();
    assert_eq!(
        engine
            .sql("MOVE FORWARD 0 FROM incremental_cursor", &[])
            .unwrap()
            .affected_rows,
        0
    );
    assert_eq!(
        integer_column(
            &engine
                .sql("SELECT currval('lazy_cursor_sequence') AS value", &[])
                .unwrap(),
            "value"
        ),
        [2]
    );
    assert_eq!(
        integer_column(
            &engine.sql("FETCH 1 FROM incremental_cursor", &[]).unwrap(),
            "value"
        ),
        [3]
    );
    assert_eq!(
        integer_column(
            &engine
                .sql("SELECT currval('lazy_cursor_sequence') AS value", &[])
                .unwrap(),
            "value"
        ),
        [3]
    );
    engine.sql("ROLLBACK", &[]).unwrap();

    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql(
            "DECLARE lazy_error CURSOR FOR SELECT 1 / divisor AS value FROM lazy_cursor_error",
            &[],
        )
        .unwrap();
    let error = engine.sql("FETCH FROM lazy_error", &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("22012"), "{error}");
    engine.sql("ROLLBACK", &[]).unwrap();

    engine.sql("SET work_mem = '64kB'; BEGIN", &[]).unwrap();
    engine
        .sql(
            "DECLARE large_cursor CURSOR FOR SELECT repeat('x', 2048) AS payload FROM generate_series(1, 512)",
            &[],
        )
        .unwrap();
    let row = engine.sql("FETCH FROM large_cursor", &[]).unwrap();
    assert_eq!(
        row.rows[0]["payload"],
        uqa_core::Value::Str("x".repeat(2048))
    );
    engine.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn pg18_cursor_declaration_validates_relations_without_evaluating_rows() {
    let engine = Engine::new();
    engine
        .sql("CREATE TABLE cursor_columns (id INTEGER)", &[])
        .unwrap();

    engine.sql("BEGIN", &[]).unwrap();
    let missing_relation = engine
        .sql(
            "DECLARE missing_relation CURSOR FOR SELECT * FROM absent_cursor_table",
            &[],
        )
        .unwrap_err();
    assert_eq!(
        missing_relation.sqlstate(),
        Some("42P01"),
        "{missing_relation}"
    );
    engine.sql("ROLLBACK", &[]).unwrap();

    engine.sql("BEGIN", &[]).unwrap();
    let missing_column = engine
        .sql(
            "DECLARE missing_column CURSOR FOR SELECT absent FROM cursor_columns",
            &[],
        )
        .unwrap_err();
    assert_eq!(missing_column.sqlstate(), Some("42703"), "{missing_column}");
    engine.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn pg18_catalog_cursor_uses_the_declare_time_relation_catalog() {
    let engine = Engine::new();
    engine
        .sql("CREATE TABLE catalog_cursor_a (id INTEGER)", &[])
        .unwrap();

    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql(
            "DECLARE catalog_cursor CURSOR FOR
             SELECT relname FROM pg_catalog.pg_class
             WHERE relname IN ('catalog_cursor_a', 'catalog_cursor_b')
             ORDER BY relname",
            &[],
        )
        .unwrap();
    engine
        .sql("CREATE TABLE catalog_cursor_b (id INTEGER)", &[])
        .unwrap();
    engine.sql("DROP TABLE catalog_cursor_a", &[]).unwrap();
    let rows = engine.sql("FETCH ALL FROM catalog_cursor", &[]).unwrap();
    assert_eq!(
        rows.rows
            .iter()
            .map(|row| row["relname"].clone())
            .collect::<Vec<_>>(),
        [uqa_core::Value::Str("catalog_cursor_a".into())]
    );
    engine.sql("ROLLBACK", &[]).unwrap();
}

fn assert_cursor_declare_search_path_bindings(engine: &Engine) {
    engine
        .sql(
            "SET search_path = public, pg_catalog; DECLARE view_path_cursor CURSOR FOR SELECT id FROM cursor_path_view; DECLARE sequence_path_cursor CURSOR FOR SELECT nextval('cursor_path_sequence') AS value; SET search_path = pg_catalog",
            &[],
        )
        .unwrap();
    assert_integer_query(engine, "FETCH ALL FROM view_path_cursor", "id", [11]);
    assert_integer_query(engine, "FETCH ALL FROM sequence_path_cursor", "value", [1]);
    engine
        .sql("SET search_path = public, pg_catalog", &[])
        .unwrap();
}

fn assert_cursor_declare_function_binding(engine: &Engine) {
    engine
        .sql(
            "DECLARE function_cursor CURSOR FOR SELECT cursor_plan_function() AS value; CREATE OR REPLACE FUNCTION cursor_plan_function() RETURNS INTEGER LANGUAGE SQL AS 'SELECT 2'",
            &[],
        )
        .unwrap();
    assert_integer_query(engine, "FETCH ALL FROM function_cursor", "value", [1]);
}

#[test]
fn pg18_cursor_keeps_declare_time_relation_bindings_and_forwards_notices() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE cursor_rename_source (id INTEGER); INSERT INTO cursor_rename_source VALUES (7); CREATE TABLE cursor_parent (id INTEGER); CREATE TABLE cursor_child (id INTEGER); INSERT INTO cursor_child VALUES (9); CREATE TABLE cursor_view_old_source (id INTEGER); INSERT INTO cursor_view_old_source VALUES (11); CREATE TABLE cursor_view_new_source (id INTEGER); INSERT INTO cursor_view_new_source VALUES (22); CREATE VIEW cursor_inner_view AS SELECT id FROM cursor_view_old_source; CREATE VIEW cursor_outer_view AS SELECT id FROM cursor_inner_view; CREATE VIEW cursor_path_view AS SELECT id FROM cursor_view_old_source; CREATE SEQUENCE cursor_path_sequence START 1; CREATE FUNCTION cursor_plan_function() RETURNS INTEGER LANGUAGE SQL AS 'SELECT 1'; CREATE FUNCTION cursor_notice() RETURNS INTEGER LANGUAGE plpgsql AS $$ BEGIN RAISE NOTICE 'from cursor'; RETURN 1; END $$",
            &[],
        )
        .unwrap();

    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql(
            "DECLARE rename_cursor CURSOR FOR SELECT id FROM cursor_rename_source",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "ALTER TABLE cursor_rename_source RENAME TO cursor_renamed_source",
            &[],
        )
        .unwrap();
    assert_eq!(
        integer_column(
            &engine.sql("FETCH ALL FROM rename_cursor", &[]).unwrap(),
            "id"
        ),
        [7]
    );

    engine
        .sql(
            "DECLARE hierarchy_cursor CURSOR FOR SELECT id FROM cursor_parent",
            &[],
        )
        .unwrap();
    engine
        .sql("ALTER TABLE cursor_child INHERIT cursor_parent", &[])
        .unwrap();
    assert!(engine
        .sql("FETCH ALL FROM hierarchy_cursor", &[])
        .unwrap()
        .rows
        .is_empty());

    engine
        .sql(
            "DECLARE view_cursor CURSOR FOR SELECT id FROM cursor_outer_view",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE OR REPLACE VIEW cursor_inner_view AS SELECT id FROM cursor_view_new_source",
            &[],
        )
        .unwrap();
    assert_eq!(
        integer_column(
            &engine.sql("FETCH ALL FROM view_cursor", &[]).unwrap(),
            "id"
        ),
        [11]
    );

    assert_cursor_declare_search_path_bindings(&engine);
    assert_cursor_declare_function_binding(&engine);

    engine
        .sql(
            "DECLARE notice_cursor CURSOR FOR SELECT cursor_notice() AS value",
            &[],
        )
        .unwrap();
    assert_eq!(
        integer_column(
            &engine.sql("FETCH ALL FROM notice_cursor", &[]).unwrap(),
            "value"
        ),
        [1]
    );
    assert_eq!(
        engine.take_sql_notices(),
        vec![("NOTICE".into(), "from cursor".into())]
    );
    engine.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn pg18_cursor_declaration_holds_access_share_until_transaction_end() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("cursor-relation-lock.db")).unwrap();
    root.sql("CREATE TABLE cursor_locked_relation (id INTEGER)", &[])
        .unwrap();
    let ddl = root.new_session().unwrap();

    root.sql("BEGIN", &[]).unwrap();
    root.sql(
        "DECLARE relation_lock_cursor CURSOR FOR SELECT id FROM cursor_locked_relation",
        &[],
    )
    .unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let ddl_thread = std::thread::spawn(move || {
        sender
            .send(ddl.sql("DROP TABLE cursor_locked_relation", &[]))
            .unwrap();
    });
    assert!(
        receiver
            .recv_timeout(std::time::Duration::from_millis(100))
            .is_err(),
        "DROP TABLE passed a cursor's AccessShare relation lock"
    );
    root.sql("ROLLBACK", &[]).unwrap();
    receiver
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("DROP TABLE remained blocked after cursor transaction end")
        .unwrap();
    ddl_thread.join().unwrap();

    root.sql(
        "CREATE TABLE cursor_locked_operator_relation (id INTEGER PRIMARY KEY, embedding VECTOR(2))",
        &[],
    )
    .unwrap();
    let operator_ddl = root.new_session().unwrap();
    root.sql("BEGIN", &[]).unwrap();
    root.sql(
        "DECLARE operator_relation_lock_cursor CURSOR FOR SELECT left_doc_id FROM vector_similarity_join(cursor_locked_operator_relation, knn_match(embedding, ARRAY[1.0, 0.0], 1), knn_match(embedding, ARRAY[1.0, 0.0], 1), 0.8)",
        &[],
    )
    .unwrap();
    let (operator_sender, operator_receiver) = std::sync::mpsc::channel();
    let operator_ddl_thread = std::thread::spawn(move || {
        operator_sender
            .send(operator_ddl.sql("DROP TABLE cursor_locked_operator_relation", &[]))
            .unwrap();
    });
    assert!(
        operator_receiver
            .recv_timeout(std::time::Duration::from_millis(100))
            .is_err(),
        "DROP TABLE passed an operator cursor's AccessShare relation lock"
    );
    root.sql("ROLLBACK", &[]).unwrap();
    operator_receiver
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("operator relation DROP TABLE remained blocked after cursor transaction end")
        .unwrap();
    operator_ddl_thread.join().unwrap();
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

    engine
        .sql(
            "CREATE SEQUENCE held_materialization_sequence START WITH 1",
            &[],
        )
        .unwrap();
    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql(
            "DECLARE volatile_held CURSOR WITH HOLD FOR SELECT nextval('held_materialization_sequence') AS value FROM generate_series(1, 3)",
            &[],
        )
        .unwrap();
    assert_eq!(
        integer_column(
            &engine.sql("FETCH 1 FROM volatile_held", &[]).unwrap(),
            "value"
        ),
        [1]
    );
    engine.sql("COMMIT", &[]).unwrap();
    assert_eq!(
        integer_column(
            &engine
                .sql(
                    "SELECT currval('held_materialization_sequence') AS value",
                    &[],
                )
                .unwrap(),
            "value"
        ),
        [4]
    );
    assert_eq!(
        integer_column(
            &engine.sql("FETCH ALL FROM volatile_held", &[]).unwrap(),
            "value"
        ),
        [3, 4]
    );
    engine.sql("CLOSE volatile_held", &[]).unwrap();

    engine
        .sql(
            "CREATE TABLE held_parent (id INTEGER PRIMARY KEY); CREATE TABLE held_child (id INTEGER PRIMARY KEY, parent_id INTEGER, CONSTRAINT held_child_parent_fk FOREIGN KEY (parent_id) REFERENCES held_parent(id) DEFERRABLE INITIALLY DEFERRED); CREATE FUNCTION insert_invalid_held_child() RETURNS INTEGER LANGUAGE SQL AS 'INSERT INTO held_child VALUES (1, 999) RETURNING id' VOLATILE",
            &[],
        )
        .unwrap();
    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql(
            "DECLARE mutating_held CURSOR WITH HOLD FOR SELECT insert_invalid_held_child() AS id",
            &[],
        )
        .unwrap();
    let error = engine.sql("COMMIT", &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("23503"), "{error}");
    assert!(engine
        .sql("SELECT id FROM held_child", &[])
        .unwrap()
        .rows
        .is_empty());
}

#[test]
fn pg18_failed_holdable_cursor_materialization_aborts_transaction() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE held_divisors (divisor INTEGER NOT NULL); INSERT INTO held_divisors VALUES (0)",
            &[],
        )
        .unwrap();
    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql(
            "DECLARE failing_held CURSOR WITH HOLD FOR SELECT 1 / divisor AS value FROM held_divisors",
            &[],
        )
        .unwrap();
    let error = engine.sql("COMMIT", &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("22012"), "{error}");
    assert_eq!(engine.transaction_depth(), 0);
    assert_integer_query(&engine, "SELECT 42 AS value", "value", [42]);
    let removed = engine.sql("FETCH ALL FROM failing_held", &[]).unwrap_err();
    assert_eq!(removed.sqlstate(), Some("34000"), "{removed}");
}

#[test]
fn cursor_volatile_routines_observe_earlier_cursor_writes() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE cursor_routine_rows (id INTEGER PRIMARY KEY); CREATE FUNCTION cursor_insert_and_count(value INTEGER) RETURNS INTEGER LANGUAGE plpgsql VOLATILE AS 'BEGIN INSERT INTO cursor_routine_rows VALUES (value); RETURN (SELECT count(*) FROM cursor_routine_rows); END'",
            &[],
        )
        .unwrap();
    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql(
            "DECLARE mutating_cursor CURSOR FOR SELECT cursor_insert_and_count(value) AS observed FROM generate_series(1, 2) AS values(value)",
            &[],
        )
        .unwrap();
    let observed = engine.sql("FETCH ALL FROM mutating_cursor", &[]).unwrap();
    assert_eq!(integer_column(&observed, "observed"), [1, 2]);
    assert_integer_query(
        &engine,
        "SELECT count(*) AS observed FROM cursor_routine_rows",
        "observed",
        [2],
    );
    engine.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn pg18_cursor_snapshot_includes_only_own_changes_visible_at_declare() {
    fn verify(engine: &Engine, declaration: &str, table: &str) {
        engine
            .sql(
                &format!(
                    "CREATE {declaration} TABLE {table} (id INTEGER PRIMARY KEY, value INTEGER NOT NULL)"
                ),
                &[],
            )
            .unwrap();
        engine
            .sql(&format!("INSERT INTO {table} VALUES (1, 10)"), &[])
            .unwrap();

        engine.sql("BEGIN", &[]).unwrap();
        engine
            .sql(&format!("UPDATE {table} SET value = 20 WHERE id = 1"), &[])
            .unwrap();
        engine
            .sql(&format!("INSERT INTO {table} VALUES (2, 20)"), &[])
            .unwrap();
        engine
            .sql(
                &format!(
                    "DECLARE own_change_cursor CURSOR FOR SELECT id, value FROM {table} ORDER BY id"
                ),
                &[],
            )
            .unwrap();
        engine
            .sql(&format!("UPDATE {table} SET value = 30 WHERE id = 1"), &[])
            .unwrap();
        engine
            .sql(&format!("DELETE FROM {table} WHERE id = 2"), &[])
            .unwrap();
        engine
            .sql(&format!("INSERT INTO {table} VALUES (3, 30)"), &[])
            .unwrap();
        let declared = engine.sql("FETCH ALL FROM own_change_cursor", &[]).unwrap();
        assert_eq!(integer_column(&declared, "id"), [1, 2]);
        assert_eq!(integer_column(&declared, "value"), [20, 20]);
        engine.sql("ROLLBACK", &[]).unwrap();

        engine.sql("BEGIN", &[]).unwrap();
        engine
            .sql(&format!("UPDATE {table} SET value = 20 WHERE id = 1"), &[])
            .unwrap();
        engine
            .sql(&format!("INSERT INTO {table} VALUES (2, 20)"), &[])
            .unwrap();
        engine
            .sql(
                &format!(
                    "DECLARE held_own_change_cursor CURSOR WITH HOLD FOR SELECT id, value FROM {table} ORDER BY id"
                ),
                &[],
            )
            .unwrap();
        let first = engine
            .sql("FETCH 1 FROM held_own_change_cursor", &[])
            .unwrap();
        assert_eq!(integer_column(&first, "id"), [1]);
        assert_eq!(integer_column(&first, "value"), [20]);
        engine
            .sql(&format!("UPDATE {table} SET value = 30 WHERE id = 1"), &[])
            .unwrap();
        engine
            .sql(&format!("DELETE FROM {table} WHERE id = 2"), &[])
            .unwrap();
        engine
            .sql(&format!("INSERT INTO {table} VALUES (3, 30)"), &[])
            .unwrap();
        engine.sql("COMMIT", &[]).unwrap();
        let remaining = engine
            .sql("FETCH ALL FROM held_own_change_cursor", &[])
            .unwrap();
        assert_eq!(integer_column(&remaining, "id"), [2]);
        assert_eq!(integer_column(&remaining, "value"), [20]);
        engine.sql("CLOSE held_own_change_cursor", &[]).unwrap();
    }

    let memory = Engine::new();
    verify(&memory, "", "memory_cursor_rows");

    let directory = tempfile::tempdir().unwrap();
    let persistent = Engine::open(&directory.path().join("cursor-own-changes.db")).unwrap();
    verify(&persistent, "", "persistent_cursor_rows");
    verify(&persistent, "TEMPORARY", "temp_cursor_rows");
}

#[test]
fn cursor_fetch_observes_session_cancellation() {
    let engine = std::sync::Arc::new(Engine::new());
    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql(
            "DECLARE cancellable_cursor CURSOR FOR SELECT value FROM generate_series(1, 1000000000) AS rows(value)",
            &[],
        )
        .unwrap();
    let canceller = {
        let engine = std::sync::Arc::clone(&engine);
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            engine.cancel();
        })
    };
    let error = engine
        .sql("FETCH ALL FROM cancellable_cursor", &[])
        .unwrap_err();
    canceller.join().unwrap();
    assert_eq!(error.sqlstate(), Some("57014"), "{error}");
    engine.reset_cancellation();
    engine.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn persistent_cursor_snapshots_do_not_consume_one_connection_per_portal() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("many-cursors.db")).unwrap();
    engine
        .sql("CREATE TABLE cursor_rows (id INTEGER)", &[])
        .unwrap();
    engine
        .sql("INSERT INTO cursor_rows VALUES (1)", &[])
        .unwrap();
    engine.sql("BEGIN", &[]).unwrap();
    for index in 0..40 {
        engine
            .sql(
                &format!("DECLARE cursor_{index} CURSOR FOR SELECT id FROM cursor_rows"),
                &[],
            )
            .unwrap();
    }
    assert_eq!(
        integer_column(&engine.sql("FETCH ALL FROM cursor_39", &[]).unwrap(), "id"),
        [1]
    );
    engine.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn fixed_and_cursor_snapshots_index_own_full_text_changes() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("cursor-fts-snapshot.db")).unwrap();
    root.sql(
        "CREATE TABLE search_rows (id INTEGER PRIMARY KEY, body TEXT, flag INTEGER)",
        &[],
    )
    .unwrap();
    root.sql(
        "CREATE INDEX search_rows_body_gin ON search_rows USING gin (body)",
        &[],
    )
    .unwrap();
    root.sql("INSERT INTO search_rows VALUES (1, 'old', 1)", &[])
        .unwrap();
    let sibling = root.new_session().unwrap();

    root.sql("BEGIN ISOLATION LEVEL REPEATABLE READ", &[])
        .unwrap();
    root.sql("SELECT count(*) FROM search_rows", &[]).unwrap();
    sibling
        .sql("INSERT INTO search_rows VALUES (4, 'newterm', 1)", &[])
        .unwrap();
    root.sql("INSERT INTO search_rows VALUES (2, 'newterm', 1)", &[])
        .unwrap();
    assert_eq!(
        integer_column(
            &root
                .sql(
                    "SELECT id FROM search_rows WHERE text_match(body, 'newterm') ORDER BY id",
                    &[],
                )
                .unwrap(),
            "id"
        ),
        [2]
    );
    root.sql(
        "DECLARE search_cursor CURSOR FOR SELECT id FROM search_rows WHERE fts_match(body, 'newterm') AND flag = 1 ORDER BY id",
        &[],
    )
    .unwrap();
    root.sql("UPDATE search_rows SET flag = 0 WHERE id = 2", &[])
        .unwrap();
    root.sql("INSERT INTO search_rows VALUES (3, 'newterm', 1)", &[])
        .unwrap();
    assert_eq!(
        integer_column(
            &root.sql("FETCH ALL FROM search_cursor", &[]).unwrap(),
            "id"
        ),
        [2]
    );
    root.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn cursor_full_text_residual_filter_uses_declare_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("cursor-fts-residual.db")).unwrap();
    root.sql(
        "CREATE TABLE cursor_filter_rows (id INTEGER PRIMARY KEY, body TEXT, flag INTEGER); CREATE INDEX cursor_filter_rows_body_gin ON cursor_filter_rows USING gin (body); INSERT INTO cursor_filter_rows VALUES (1, 'newterm', 1)",
        &[],
    )
    .unwrap();
    let sibling = root.new_session().unwrap();
    root.sql("BEGIN", &[]).unwrap();
    root.sql(
        "DECLARE filter_snapshot_cursor CURSOR FOR SELECT id FROM cursor_filter_rows WHERE fts_match(body, 'newterm') AND flag = 1",
        &[],
    )
    .unwrap();
    sibling
        .sql("UPDATE cursor_filter_rows SET flag = 0 WHERE id = 1", &[])
        .unwrap();
    assert_integer_query(&root, "FETCH ALL FROM filter_snapshot_cursor", "id", [1]);
    root.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn cursor_full_text_validation_uses_declare_snapshot_after_relation_rename() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("cursor-fts-rename.db")).unwrap();
    engine
        .sql(
            "CREATE TABLE cursor_search_rows (id INTEGER PRIMARY KEY, body TEXT); CREATE INDEX cursor_search_rows_body_gin ON cursor_search_rows USING gin (body); INSERT INTO cursor_search_rows VALUES (1, 'newterm')",
            &[],
        )
        .unwrap();
    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql(
            "DECLARE renamed_search_cursor CURSOR FOR SELECT id FROM cursor_search_rows WHERE text_match(body, 'newterm')",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "ALTER TABLE cursor_search_rows RENAME TO cursor_search_rows_renamed",
            &[],
        )
        .unwrap();
    assert_integer_query(&engine, "FETCH ALL FROM renamed_search_cursor", "id", [1]);
    engine.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn pg18_relation_oid_survives_rename_truncate_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("relation-oid-lifecycle.db");
    let engine = Engine::open(&path).unwrap();
    engine
        .sql("CREATE TABLE relation_oid_source (id INTEGER)", &[])
        .unwrap();
    let original = integer_column(
        &engine
            .sql("SELECT 'relation_oid_source'::regclass AS oid", &[])
            .unwrap(),
        "oid",
    )[0];
    engine
        .sql(
            "ALTER TABLE relation_oid_source RENAME TO relation_oid_renamed; TRUNCATE relation_oid_renamed",
            &[],
        )
        .unwrap();
    assert_integer_query(
        &engine,
        "SELECT 'relation_oid_renamed'::regclass AS oid",
        "oid",
        [original],
    );
    drop(engine);

    let reopened = Engine::open(&path).unwrap();
    assert_integer_query(
        &reopened,
        "SELECT 'relation_oid_renamed'::regclass AS oid",
        "oid",
        [original],
    );
    reopened
        .sql(
            "DROP TABLE relation_oid_renamed; CREATE TABLE relation_oid_renamed (id INTEGER)",
            &[],
        )
        .unwrap();
    let recreated = integer_column(
        &reopened
            .sql("SELECT 'relation_oid_renamed'::regclass AS oid", &[])
            .unwrap(),
        "oid",
    )[0];
    assert_ne!(recreated, original);
}

#[test]
fn cursor_snapshot_value_indexes_exclude_post_declare_changes() {
    fn verify(engine: &Engine, table: &str) {
        engine
            .sql(
                &format!("CREATE TABLE {table} (id INTEGER PRIMARY KEY, value INTEGER)"),
                &[],
            )
            .unwrap();
        engine
            .sql(&format!("INSERT INTO {table} VALUES (1, 10)"), &[])
            .unwrap();

        engine.sql("BEGIN", &[]).unwrap();
        engine
            .sql(
                &format!(
                    "DECLARE existing_index_cursor CURSOR FOR SELECT id FROM {table} WHERE id = 1"
                ),
                &[],
            )
            .unwrap();
        engine
            .sql(&format!("DELETE FROM {table} WHERE id = 1"), &[])
            .unwrap();
        assert_eq!(
            integer_column(
                &engine
                    .sql("FETCH ALL FROM existing_index_cursor", &[])
                    .unwrap(),
                "id"
            ),
            [1]
        );
        engine.sql("ROLLBACK", &[]).unwrap();

        engine.sql("BEGIN", &[]).unwrap();
        engine
            .sql(
                &format!(
                    "DECLARE absent_index_cursor CURSOR FOR SELECT id FROM {table} WHERE id = 2"
                ),
                &[],
            )
            .unwrap();
        engine
            .sql(&format!("INSERT INTO {table} VALUES (2, 20)"), &[])
            .unwrap();
        assert!(engine
            .sql("FETCH ALL FROM absent_index_cursor", &[])
            .unwrap()
            .rows
            .is_empty());
        engine.sql("ROLLBACK", &[]).unwrap();
    }

    verify(&Engine::new(), "memory_index_cursor_rows");
    let directory = tempfile::tempdir().unwrap();
    let persistent = Engine::open(&directory.path().join("cursor-index-snapshot.db")).unwrap();
    verify(&persistent, "persistent_index_cursor_rows");
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
    let zero = engine
        .sql("FETCH FORWARD 0 FROM zero_position", &[])
        .unwrap();
    assert_eq!(zero.columns, ["x"]);
    assert_eq!(zero.column_types, [Some(ColumnType::Integer)]);
    assert!(zero.rows.is_empty());
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

fn assert_pg18_vacuum_validation_errors(eng: &Engine) {
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
}

#[test]
fn pg18_vacuum_runs_outside_transactions_and_preserves_error_precedence() {
    let directory = tempfile::tempdir().unwrap();
    let eng = Engine::open(&directory.path().join("vacuum.db")).unwrap();
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
    assert_eq!(
        integer_column(&eng.sql("SELECT a FROM vacuum_target", &[]).unwrap(), "a"),
        [1]
    );
    eng.sql("UPDATE vacuum_target SET b = 'rewritten' WHERE a = 1", &[])
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
    eng.sql("SET default_transaction_read_only = on", &[])
        .unwrap();
    eng.sql("VACUUM (ANALYZE) vacuum_target", &[]).unwrap();
    eng.sql("RESET default_transaction_read_only", &[]).unwrap();

    eng.sql(
        "CREATE TABLE vacuum_parent (a INTEGER, b TEXT) PARTITION BY RANGE (a); CREATE TABLE vacuum_child PARTITION OF vacuum_parent FOR VALUES FROM (0) TO (10); INSERT INTO vacuum_parent VALUES (1, 'one'), (2, 'two')",
        &[],
    )
    .unwrap();
    eng.sql("VACUUM (ANALYZE) ONLY vacuum_parent (a)", &[])
        .unwrap();
    let parent_only = eng.column_stats("vacuum_parent").unwrap();
    assert_eq!(parent_only["a"].row_count, 0);
    assert!(!parent_only.contains_key("b"));
    eng.sql("VACUUM (ANALYZE) vacuum_parent (b)", &[]).unwrap();
    let descendants = eng.column_stats("vacuum_parent").unwrap();
    assert_eq!(descendants["a"].row_count, 0);
    assert_eq!(descendants["b"].row_count, 2);

    assert_pg18_vacuum_validation_errors(&eng);

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
fn pg18_vacuum_full_waits_for_relation_holders_before_reserving_the_backend_writer() {
    let directory = tempfile::tempdir().unwrap();
    let root =
        std::sync::Arc::new(Engine::open(&directory.path().join("vacuum-locks.db")).unwrap());
    root.sql(
        "CREATE TABLE vacuum_lock_rows (id INTEGER PRIMARY KEY, value INTEGER); INSERT INTO vacuum_lock_rows VALUES (1, 10)",
        &[],
    )
    .unwrap();
    let vacuum = std::sync::Arc::new(root.new_session().unwrap());
    root.sql("BEGIN", &[]).unwrap();
    root.sql("SELECT * FROM vacuum_lock_rows FOR UPDATE", &[])
        .unwrap();

    let (sender, receiver) = std::sync::mpsc::channel();
    let vacuum_thread = std::thread::spawn(move || {
        sender
            .send(vacuum.sql("VACUUM FULL vacuum_lock_rows", &[]))
            .unwrap();
    });
    std::thread::sleep(std::time::Duration::from_millis(100));
    root.sql("UPDATE vacuum_lock_rows SET value = 20 WHERE id = 1", &[])
        .unwrap();
    assert!(receiver.try_recv().is_err());
    root.sql("COMMIT", &[]).unwrap();
    receiver
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap()
        .unwrap();
    vacuum_thread.join().unwrap();
    assert_eq!(
        integer_column(
            &root
                .sql("SELECT value FROM vacuum_lock_rows WHERE id = 1", &[])
                .unwrap(),
            "value",
        ),
        [20]
    );
}

#[test]
fn pg18_vacuum_full_reclaims_persistent_file_space() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("vacuum-full-size.db");
    {
        let engine = Engine::open(&path).unwrap();
        engine
            .sql(
                "CREATE TABLE vacuum_size_rows (id INTEGER PRIMARY KEY, value TEXT); INSERT INTO vacuum_size_rows SELECT x, repeat('x', 2048) FROM generate_series(1, 2000) AS rows(x); DELETE FROM vacuum_size_rows WHERE id > 10",
                &[],
            )
            .unwrap();
    }
    let size_before = std::fs::metadata(&path).unwrap().len();

    {
        let engine = Engine::open(&path).unwrap();
        engine.sql("VACUUM FULL vacuum_size_rows", &[]).unwrap();
    }
    let size_after = std::fs::metadata(&path).unwrap().len();
    assert!(
        size_after < size_before / 2,
        "VACUUM FULL did not reclaim enough space: before={size_before}, after={size_after}"
    );
}

#[test]
fn pg18_vacuum_full_rewrites_compressed_storage() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("vacuum-full-compressed.uqac.sqlite3");
    let engine =
        Engine::open_compressed(&path, uqa_storage::SQLiteCompressionOptions::default()).unwrap();
    engine
        .sql(
            "CREATE TABLE vacuum_compressed_rows (id INTEGER PRIMARY KEY, value TEXT); INSERT INTO vacuum_compressed_rows SELECT x, repeat('x', 1024) FROM generate_series(1, 500) AS rows(x); DELETE FROM vacuum_compressed_rows WHERE id > 2",
            &[],
        )
        .unwrap();
    engine
        .sql("VACUUM FULL vacuum_compressed_rows", &[])
        .unwrap();
    let rows = engine
        .sql("SELECT id FROM vacuum_compressed_rows ORDER BY id", &[])
        .unwrap();
    assert_eq!(integer_column(&rows, "id"), [1, 2]);
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
fn pg18_xmin_reads_legacy_persistent_tuple_metadata_through_qualified_references() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("legacy-xmin.db");
    let expected_xmin = {
        let eng = Engine::open(&path).unwrap();
        eng.sql("CREATE TABLE versioned (a INTEGER)", &[]).unwrap();
        eng.sql("INSERT INTO versioned VALUES (1)", &[]).unwrap();
        integer_column(&eng.sql("SELECT xmin FROM versioned", &[]).unwrap(), "xmin")[0]
    };

    let connection = rusqlite::Connection::open(&path).unwrap();
    let body: String = connection
        .query_row("SELECT body FROM _documents WHERE doc_id = 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    let mut legacy: Document = serde_json::from_str(&body).unwrap();
    assert_eq!(
        legacy.remove("\0uqa.system.xmin"),
        Some(uqa_core::Value::Int(expected_xmin))
    );
    connection
        .execute(
            "UPDATE _documents SET body = ?1 WHERE doc_id = 1",
            [serde_json::to_string(&legacy).unwrap()],
        )
        .unwrap();
    drop(connection);

    let eng = Engine::open(&path).unwrap();
    assert_eq!(
        integer_column(&eng.sql("SELECT xmin FROM versioned", &[]).unwrap(), "xmin",),
        [expected_xmin]
    );
    assert_eq!(
        integer_column(
            &eng.sql("SELECT v.xmin FROM versioned AS v", &[]).unwrap(),
            "xmin",
        ),
        [expected_xmin]
    );
}

#[test]
fn legacy_user_xmin_values_are_not_overwritten_by_tuple_version_metadata() {
    let eng = Engine::new();
    eng.create_default_table("legacy_xmin", Vec::new()).unwrap();
    eng.register_column(
        "legacy_xmin",
        uqa_sql::ast::ColumnDef {
            name: "xmin".into(),
            ty: uqa_sql::ast::ColumnType::Text,
            object_id: None,
            missing_value: None,
            primary_key: false,
            not_null: false,
            not_null_explicit: false,
            not_null_name: None,
            not_null_validated: true,
            not_null_no_inherit: false,
            auto_increment: None,
            unique: false,
            default: None,
            generated: None,
            check: None,
            check_name: None,
            check_enforced: true,
            check_validated: true,
            check_no_inherit: false,
            references: None,
        },
    )
    .unwrap();
    eng.add_document(
        "legacy_xmin",
        1,
        Document::from([("xmin".into(), uqa_core::Value::Str("user value".into()))]),
    )
    .unwrap();
    let document = eng.get_document("legacy_xmin", 1).unwrap().unwrap();
    assert_eq!(
        document.get("xmin"),
        Some(&uqa_core::Value::Str("user value".into()))
    );
    assert_eq!(
        document.len(),
        1,
        "internal tuple metadata leaked: {document:?}"
    );
}

#[test]
fn schemaless_system_xmin_mirror_is_refreshed_without_becoming_user_data() {
    let eng = Engine::new();
    eng.create_default_table("schemaless_xmin", Vec::new())
        .unwrap();
    eng.add_document(
        "schemaless_xmin",
        1,
        Document::from([("value".into(), uqa_core::Value::Int(10))]),
    )
    .unwrap();
    let first = eng.get_document("schemaless_xmin", 1).unwrap().unwrap()["xmin"].clone();
    let updated = eng
        .sql("UPDATE schemaless_xmin SET value = 20 RETURNING xmin", &[])
        .unwrap();
    assert_ne!(updated.rows[0]["xmin"], first);
    let document = eng.get_document("schemaless_xmin", 1).unwrap().unwrap();
    assert_ne!(document["xmin"], first);
    let uqa_core::Value::Int(expected_xmin) = document["xmin"] else {
        panic!("system xmin is not an integer: {document:?}");
    };
    assert_eq!(
        integer_column(
            &eng.sql("SELECT xmin FROM schemaless_xmin", &[]).unwrap(),
            "xmin"
        ),
        [expected_xmin]
    );
}

#[test]
fn schemaless_user_xmin_equal_to_tuple_version_is_not_misclassified_as_a_mirror() {
    let eng = Engine::new();
    eng.create_default_table("schemaless_user_xmin", Vec::new())
        .unwrap();
    eng.add_document(
        "schemaless_user_xmin",
        1,
        Document::from([("value".into(), uqa_core::Value::Int(10))]),
    )
    .unwrap();
    let first_system_xmin = integer_column(
        &eng.sql(
            "SELECT xmin FROM schemaless_user_xmin WHERE value = 10",
            &[],
        )
        .unwrap(),
        "xmin",
    )[0];
    let colliding_user_xmin = first_system_xmin + 1;
    eng.add_document(
        "schemaless_user_xmin",
        2,
        Document::from([
            ("xmin".into(), uqa_core::Value::Int(colliding_user_xmin)),
            ("value".into(), uqa_core::Value::Int(20)),
        ]),
    )
    .unwrap();
    assert_eq!(
        integer_column(
            &eng.sql(
                "SELECT xmin FROM schemaless_user_xmin WHERE value = 20",
                &[],
            )
            .unwrap(),
            "xmin",
        ),
        [colliding_user_xmin]
    );
    eng.update_document_fields(
        "schemaless_user_xmin",
        2,
        std::collections::BTreeMap::from([("value".into(), uqa_core::Value::Int(30))]),
        std::collections::BTreeMap::new(),
    )
    .unwrap();
    let document = eng
        .get_document("schemaless_user_xmin", 2)
        .unwrap()
        .unwrap();
    assert_eq!(
        document,
        Document::from([
            ("value".into(), uqa_core::Value::Int(30)),
            ("xmin".into(), uqa_core::Value::Int(colliding_user_xmin)),
        ])
    );
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
