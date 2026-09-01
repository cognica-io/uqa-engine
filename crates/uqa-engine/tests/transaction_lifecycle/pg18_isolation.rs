//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

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
        "SELECT setval('permanent_sequence', 42, false)",
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
    eng.sql("SELECT setval('temporary_sequence', 84, false)", &[])
        .unwrap();
    assert_eq!(
        integer_column(
            &eng.sql("SELECT nextval('temporary_sequence') AS value", &[])
                .unwrap(),
            "value"
        ),
        [84]
    );
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
