//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[path = "pg18_cursors/movement.rs"]
mod movement;
#[path = "pg18_cursors/relation_locks.rs"]
mod relation_locks;
#[path = "pg18_cursors/volatile_scrolling.rs"]
mod volatile_scrolling;

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
fn pg18_cursor_evaluates_offset_target_rows_and_stops_at_limit() {
    let engine = Engine::new();
    engine
        .sql("CREATE SEQUENCE offset_cursor_sequence; BEGIN", &[])
        .unwrap();
    engine
        .sql(
            "DECLARE offset_cursor CURSOR FOR SELECT nextval('offset_cursor_sequence') AS value FROM generate_series(1, 4) OFFSET 2 LIMIT 1",
            &[],
        )
        .unwrap();
    assert_eq!(
        integer_column(
            &engine.sql("FETCH FROM offset_cursor", &[]).unwrap(),
            "value"
        ),
        [3]
    );
    assert_eq!(
        integer_column(
            &engine
                .sql("SELECT currval('offset_cursor_sequence') AS value", &[])
                .unwrap(),
            "value"
        ),
        [3]
    );
    assert!(engine
        .sql("FETCH ALL FROM offset_cursor", &[])
        .unwrap()
        .rows
        .is_empty());
    assert_eq!(
        integer_column(
            &engine
                .sql("SELECT currval('offset_cursor_sequence') AS value", &[])
                .unwrap(),
            "value"
        ),
        [3]
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
